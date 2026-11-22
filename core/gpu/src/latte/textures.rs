//! Part of [`super`]: see its documentation.
//!
//! ## Where this file sits
//!
//! ```text
//! Game files ─► Loader ─► CPU ◄─► Memory ◄─► OS (HLE) ─► GPU ─► Screen
//!                                                        ▲▲▲  here
//! Controllers, clock ─► OS (HLE) ─► Sound ─► Speakers
//!
//! ┌────────────────────┐   ┌──────────────────────────┐   ┌────────────────────┐
//! │ GPU register state │   │ latte/textures.rs        │   │ textures, buffers, │
//! │ draw commands      │──►│ ──────────────────────── │──►│ draws              │
//! │                    │   │ draws → host work:       │   │                    │
//! │                    │   │ textures                 │   │                    │
//! └────────────────────┘   └──────────────────────────┘   └────────────────────┘
//! ```
//!
//! The "textures" share of latte.rs (draws → host work), moved here so that no file is longer
//! than 500 lines. It works exactly as if it were still written in that file. The core of the
//! GPU emulation: for each draw it gathers the game's shaders, textures and buffers and
//! describes the same draw for your graphics card. Terms such as HLE, JIT or shader are
//! explained in `docs/guide/wiiu_term.md`.

use super::*;

/// Widens guest texel formats the host lacks; `None` means bytes pass through.
pub(super) fn widen_texels(gx2_format: u32, data: &[u8]) -> Option<Vec<u8>> {
    let scale5 = |v: u16| ((u32::from(v) * 255 + 15) / 31) as u8;
    let scale6 = |v: u16| ((u32::from(v) * 255 + 31) / 63) as u8;
    let scale4 = |v: u16| (v * 17) as u8;
    let pixels16 = || data.as_chunks::<2>().0.iter().map(|p| u16::from_le_bytes([p[0], p[1]]));
    match gx2_format & 0xFF {
        // R in the low bits (see Cemu's "Vulkan has R in MSB, GPU7 has it in LSB").
        0x08 => Some(
            pixels16()
                .flat_map(|v| [scale5(v & 0x1F), scale6((v >> 5) & 0x3F), scale5(v >> 11), 255])
                .collect(),
        ),
        0x09 => Some(
            pixels16()
                .flat_map(|v| [scale6(v & 0x3F), scale5((v >> 6) & 0x1F), scale5(v >> 11), 255])
                .collect(),
        ),
        0x0A => Some(
            pixels16()
                .flat_map(|v| {
                    [
                        scale5(v & 0x1F),
                        scale5((v >> 5) & 0x1F),
                        scale5((v >> 10) & 0x1F),
                        if v >> 15 != 0 { 255 } else { 0 },
                    ]
                })
                .collect(),
        ),
        0x0C => Some(
            pixels16()
                .flat_map(|v| {
                    [
                        scale5(v >> 11),
                        scale5((v >> 6) & 0x1F),
                        scale5((v >> 1) & 0x1F),
                        if v & 1 != 0 { 255 } else { 0 },
                    ]
                })
                .collect(),
        ),
        0x0B => Some(
            pixels16()
                .flat_map(|v| {
                    [
                        scale4(v & 0xF),
                        scale4((v >> 4) & 0xF),
                        scale4((v >> 8) & 0xF),
                        scale4(v >> 12),
                    ]
                })
                .collect(),
        ),
        0x02 => Some(data.iter().flat_map(|v| [(v & 0xF) * 17, (v >> 4) * 17]).collect()),
        0x1B => Some(
            data.as_chunks::<4>()
                .0
                .iter()
                .flat_map(|p| {
                    let v = u32::from_le_bytes([p[0], p[1], p[2], p[3]]);
                    let swapped = (v >> 22)
                        | (((v >> 12) & 0x3FF) << 10)
                        | (((v >> 2) & 0x3FF) << 20)
                        | ((v & 3) << 30);
                    swapped.to_le_bytes()
                })
                .collect(),
        ),
        _ => None,
    }
}

pub(super) fn dim_of(gx2_dim: u32) -> TextureDim {
    match gx2_dim {
        addrlib::dim::D3 => TextureDim::D3,
        addrlib::dim::CUBE => TextureDim::Cube,
        addrlib::dim::D1_ARRAY | addrlib::dim::D2_ARRAY | addrlib::dim::D2_ARRAY_MSAA => {
            TextureDim::D2Array
        }
        _ => TextureDim::D2,
    }
}

impl WriteTracker {
    pub(super) fn note(&mut self, address: u32, size: u32) {
        if size == 0 {
            return;
        }
        if self.stamps.is_empty() {
            self.stamps = vec![0; 1 << (32 - TRACK_PAGE_BITS)];
            self.coarse = vec![0; 1 << (32 - TRACK_COARSE_BITS)];
        }
        self.now = self.now.saturating_add(1);
        let end = address.saturating_add(size - 1);
        let first = (address >> TRACK_PAGE_BITS) as usize;
        let last = (end >> TRACK_PAGE_BITS) as usize;
        self.stamps[first..=last].fill(self.now);
        let first = (address >> TRACK_COARSE_BITS) as usize;
        let last = (end >> TRACK_COARSE_BITS) as usize;
        self.coarse[first..=last].fill(self.now);
    }

    /// Whether nothing in `[address, address + size)` was written after `stamp`.
    pub(super) fn unchanged_since(&self, address: u32, size: u32, stamp: u32) -> bool {
        if self.stamps.is_empty() || size == 0 {
            return true;
        }
        let end = address.saturating_add(size - 1);
        let coarse = (address >> TRACK_COARSE_BITS) as usize..=(end >> TRACK_COARSE_BITS) as usize;
        if self.coarse[coarse].iter().all(|&written| written <= stamp) {
            return true;
        }
        let first = (address >> TRACK_PAGE_BITS) as usize;
        let last = (end >> TRACK_PAGE_BITS) as usize;
        self.stamps[first..=last].iter().all(|&written| written <= stamp)
    }
}

/// Bytes of a texel of `format` when the host keeps its texels exactly as the guest does
/// (uploads copy them unchanged): what the renderer reads back of a target in this format
/// can go to guest memory as it is.
pub(super) const fn readback_bytes(format: u32) -> Option<u32> {
    match format {
        0x001 => Some(1),
        0x005 | 0x007 | 0x806 => Some(2),
        0x00F | 0x01A | 0x41A | 0x80E | 0x810 => Some(4),
        0x01F | 0x81E | 0x820 => Some(8),
        0x823 => Some(16),
        _ => None,
    }
}

/// Small tiled render targets whose values the CPU reads (Cemu's
/// `LatteTexture_ShouldEnableReadback`): Xenoblade Chronicles X's 1x1 `R16_FLOAT` scene
/// luminance (allocated 16x8), which sets its exposure, and Cars 3's `R32_FLOAT` exposure.
/// Never read back, the CPU saw no light at all and lit the picture white.
pub(super) fn small_tiled_readback(words: &SurfaceWords) -> Option<crate::tiling::Layout> {
    /// `R16_FLOAT` / `R32_FLOAT`.
    const R16_FLOAT: u32 = 0x806;
    const R32_FLOAT: u32 = 0x80E;
    let in_mem1 = words.address < 0x0200_0000;
    let wanted = match words.format {
        R32_FLOAT => words.width == 1 && words.height <= 8,
        R16_FLOAT => words.width <= 16 && words.height <= 8 && !in_mem1,
        _ => false,
    };
    if words.tile_mode != tile_mode::TM_1D_TILED_THIN1 || words.depth > 1 || !wanted {
        return None;
    }
    let (pipe_swizzle, bank_swizzle) = crate::tiling::swizzle_from_address(words.address);
    Some(crate::tiling::Layout {
        tile_mode: words.tile_mode,
        bpp: if words.format == R16_FLOAT { 16 } else { 32 },
        pitch: words.pitch.max(8),
        height: words.height.next_multiple_of(8),
        samples: 1,
        is_depth: false,
        pipe_swizzle,
        bank_swizzle,
    })
}

impl Default for Readbacks {
    fn default() -> Self {
        let (reply, results) = std::sync::mpsc::channel();
        Self { reply, results, pending: Vec::new() }
    }
}

impl std::fmt::Debug for Readbacks {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Readbacks").field("pending", &self.pending).finish_non_exhaustive()
    }
}

impl Default for Latte {
    fn default() -> Self {
        Self {
            commands: Vec::new(),
            color_targets: [SurfaceWords::default(); 8],
            depth_target: SurfaceWords::default(),
            frame: 0,
            arena_cursor: 0,
            arena_entries: HashMap::new(),
            guest_buffers: HashMap::new(),
            index_buffers: HashMap::new(),
            writes: WriteTracker::default(),
            buffer_cache: std::env::var_os("SHOCORA_NO_BUFFER_CACHE").is_none(),
            host_surfaces: HashMap::new(),
            requested_factor: 1,
            texture_rules: Vec::new(),
            sampler_rules: Vec::new(),
            tv_width: 0,
            resolution: ResolutionScale::default(),
            textures: HashMap::new(),
            programs: HashMap::new(),
            shader_uses: HashMap::new(),
            fetch_cache: HashMap::new(),
            key_buffer: Vec::new(),
            code_cache: HashMap::new(),
            next_module_id: 1,
            stream_buffers: Vec::new(),
            stream_cursor: 0,
            readbacks: Readbacks::default(),
            skipped_draws: 0,
        }
    }
}

pub(super) fn unit_mask(units: &[u8]) -> u32 {
    units.iter().fold(0, |mask, unit| mask | (1 << unit))
}

/// Swizzle bits never take part in a surface's identity.
pub(super) const fn masked_address(address: u32, tile: u32) -> u32 {
    if tile_mode::is_macro_tiled(tile) { address & !0x7FF } else { address }
}
impl Latte {
    pub fn end_frame(&mut self) {
        self.frame += 1;
        if self.frame.is_multiple_of(TEXTURE_SWEEP_FRAMES) {
            self.forget_unused_textures();
        }
    }

    /// Frees the host's copies of textures that were not used for long.
    pub(super) fn forget_unused_textures(&mut self) {
        let frame = self.frame;
        let targets = &self.host_surfaces;
        let expired: Vec<SurfaceKey> = self
            .textures
            .iter()
            .filter(|(key, known)| {
                frame.wrapping_sub(known.last_check) >= TEXTURE_EXPIRE_FRAMES
                    && !targets.contains_key(&(key.address, key.width, key.height))
            })
            .take(MAX_TEXTURES_FORGOTTEN)
            .map(|(key, _)| *key)
            .collect();
        for key in expired {
            self.textures.remove(&key);
            self.commands.push(RenderCommand::ForgetTexture(key));
        }
    }

    pub(super) fn arena_reserve(&mut self, size: u32, align: u32) -> u32 {
        let mut offset = self.arena_cursor.next_multiple_of(align);
        if u64::from(offset) + u64::from(size) > u64::from(STREAM_REGION_BASE) {
            self.commands.push(RenderCommand::ArenaWrap);
            self.arena_entries.clear();
            self.guest_buffers.clear();
            self.index_buffers.clear();
            offset = 0;
        }
        self.arena_cursor = offset + size;
        offset
    }

    /// Appends bytes, reusing an identical copy if there is one.
    pub(super) fn arena_store(&mut self, data: &[u8], align: u32) -> u32 {
        if data.is_empty() {
            return 0;
        }
        let hash = xxh3_64(data) ^ (u64::from(align) << 56) ^ (data.len() as u64).rotate_left(23);
        if let Some(slice) = self.arena_entries.get(&hash) {
            return slice.offset;
        }
        let offset = self.arena_reserve(data.len() as u32, align);
        self.commands.push(RenderCommand::ArenaWrite { offset, bytes: Arc::from(data) });
        self.arena_entries.insert(hash, ArenaSlice { offset });
        offset
    }

    /// Every surface that exists on the host only.
    pub fn host_surface_keys(&self) -> Vec<SurfaceKey> {
        self.host_surfaces.values().copied().collect()
    }

    /// Brings back what [`Self::host_surface_keys`] named, after the renderer was reset.
    pub fn restore_surfaces(&mut self, surfaces: Vec<TextureUpload>) {
        for surface in surfaces {
            let key = surface.key;
            self.host_surfaces.insert((key.address, key.width, key.height), key);
            self.commands.push(RenderCommand::Upload(Box::new(surface)));
        }
    }

    /// The CPU wrote `[address, address + size)` (effective) for the GPU to read.
    pub fn note_cpu_write(&mut self, address: u32, size: u32) {
        self.writes.note(address, size);
    }

    /// [`arena_store`](Self::arena_store) of guest memory at `effective`: hashed again
    /// only after the title wrote there, like Cemu's buffer cache does.
    pub(super) fn arena_store_guest(&mut self, effective: u32, data: &[u8], align: u32) -> u32 {
        let key = (effective, data.len() as u32, align);
        if !self.buffer_cache {
            return self.arena_store(data, align);
        }
        if let Some(known) = self.guest_buffers.get(&key)
            && self.frame.wrapping_sub(known.checked) < BUFFER_RECHECK_FRAMES
            && self.writes.unchanged_since(effective, key.1, known.stamp)
        {
            return known.offset;
        }
        let stamp = self.writes.now;
        let offset = self.arena_store(data, align);
        self.guest_buffers.insert(key, GuestBuffer { offset, stamp, checked: self.frame });
        offset
    }

    /// Indices of two triangles per rect. The second one is chosen in the vertex
    /// shader, which alone knows the corner positions: see `RECT_ENTRY_POINT`.
    pub(super) fn rect_corners(&mut self, list: &[u32], base_vertex: u32) -> Vec<u32> {
        let corners: Vec<u32> = list.iter().map(|i| i.wrapping_add(base_vertex)).collect();
        let bytes: Vec<u8> = corners.iter().flat_map(|i| i.to_le_bytes()).collect();
        let first_word = self.arena_store(&bytes, 4) >> 2;
        let mut out = Vec::with_capacity(corners.len() * 2);
        for (rect, triangle) in corners.as_chunks::<3>().0.iter().enumerate() {
            let word = first_word + rect as u32 * 3;
            out.extend_from_slice(triangle);
            out.extend((0..3).map(|corner| {
                shader::RECT_CORNER_FLAG | (corner << shader::RECT_CORNER_SHIFT) | word
            }));
        }
        out
    }

    pub(super) fn arena_write_unique(&mut self, data: &[u8], align: u32) -> u32 {
        let offset = self.arena_reserve(data.len() as u32, align);
        self.commands.push(RenderCommand::ArenaWrite { offset, bytes: Arc::from(data) });
        offset
    }

    pub(super) fn key_of_target(words: &SurfaceWords, is_depth: bool) -> SurfaceKey {
        let format =
            if is_depth { host_depth_format(words.format) } else { host_format(words.format) };
        SurfaceKey {
            address: masked_address(words.address, words.tile_mode),
            width: words.width,
            height: words.height,
            layers: words.depth.max(1),
            format,
        }
    }

    /// A draw to `words`: read back at the flip if it is in linear memory.
    pub(super) fn note_readback(&mut self, words: &SurfaceWords, surface: SurfaceKey) {
        let tiled = small_tiled_readback(words);
        if (words.tile_mode != tile_mode::TM_LINEAR_ALIGNED && tiled.is_none())
            || words.level != 0
            || words.slice != 0
            || words.aa != 0
            || surface.layers != 1
            || self.readbacks.pending.iter().any(|target| target.surface == surface)
        {
            return;
        }
        let Some(bytes) = readback_bytes(words.format) else { return };
        if let Ok(only) = std::env::var("TMP_READBACK_ONLY")
            && !only.split(',').any(|f| u32::from_str_radix(f, 16) == Ok(words.format))
        {
            return;
        }
        let Some(address) = wiiu_mem_addr::physical_to_effective(words.address) else { return };
        self.readbacks.pending.push(ReadbackTarget {
            surface,
            address,
            row_pitch: words.pitch * bytes,
            tiled,
        });
    }

    /// Asks the renderer for the linear targets drawn since the last flip.
    pub fn flush_readbacks(&mut self) {
        for target in self.readbacks.pending.drain(..) {
            let reply = ReadbackReply(self.readbacks.reply.clone());
            self.commands.push(RenderCommand::Readback { target, reply });
        }
    }

    /// Writes what the renderer has read back so far to guest memory.
    pub fn apply_readbacks(&mut self, memory: &mut Memory) {
        while let Ok(data) = self.readbacks.results.try_recv() {
            let target = data.target;
            let row_bytes = data.rows.len() / target.surface.height.max(1) as usize;
            if row_bytes == 0 {
                continue;
            }
            if let Some(layout) = target.tiled {
                let texel = (layout.bpp / 8) as usize;
                for (y, row) in (0..).zip(data.rows.chunks_exact(row_bytes)) {
                    for (x, bytes) in (0..).zip(row.chunks_exact(texel)) {
                        let address = target.address + layout.address(x, y, 0);
                        if memory.write_bytes(address, bytes).is_ok() {
                            self.note_cpu_write(address, bytes.len() as u32);
                        }
                    }
                }
                continue;
            }
            for (row, bytes) in data.rows.chunks_exact(row_bytes).enumerate() {
                let address = target.address + row as u32 * target.row_pitch;
                if memory.write_bytes(address, bytes).is_ok() {
                    // Guest buffers there are read again.
                    self.note_cpu_write(address, bytes.len() as u32);
                }
            }
        }
    }
}
