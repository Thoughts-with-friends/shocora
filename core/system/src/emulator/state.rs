//! Saved states: the whole guest machine in a file, one per title and slot.
//!
//! Guest memory goes in as compressed runs of non-zero pages, everything else through serde.
//! What the GPU drew exists on the host only, so the renderer is asked for it first (see
//! [`Emulator::request_surfaces`]). Translated code and the GPU's caches are not saved: they are
//! made again from guest memory.
//! Save data on the host's disk is not part of a state, and a `zlib` stream the title has open
//! does not come back. A state only loads into the title that saved it, built the same way.
use std::{
    path::{Path, PathBuf},
    sync::atomic::{AtomicUsize, Ordering},
};

use serde_bytes::ByteBuf;
use wiiu_cafe_os::{CafeOs, registry::RegistryState};
use wiiu_cpu::{CpuState, Jit};
use wiiu_gpu::{GpuState, render::TextureUpload};
use wiiu_iosu::Iosu;
use wiiu_memory::Memory;
use wiiu_sound::speed::SpeedResampler;

use super::{Emulator, Status};
use crate::EmuError;

/// Slots per title: `0` to `5`.
pub const STATE_SLOTS: u8 = 6;

/// Changes whenever old files can no longer be read.
const FORMAT: u32 = 1;
const PAGE: usize = 0x1000;
/// Longest run compressed as one piece: what makes compressing on several threads possible.
const MAX_RUN: usize = 0x100_0000;
const THREADS: usize = 4;

#[derive(serde::Serialize, serde::Deserialize)]
struct StateFile {
    format: u32,
    title_id: Option<u64>,
    /// [`SavedMachine`] as `postcard`, compressed.
    machine: ByteBuf,
    memory: Vec<MemoryRun>,
}

#[derive(serde::Serialize, serde::Deserialize)]
struct MemoryRun {
    address: u32,
    compressed: ByteBuf,
}

/// Field for field what [`SavedMachine`] reads back.
#[derive(serde::Serialize)]
struct MachineRef<'a> {
    cores: &'a [CpuState],
    os: &'a CafeOs,
    registry: RegistryState,
    iosu: &'a Iosu,
    gpu: GpuState,
}

#[derive(serde::Deserialize)]
struct SavedMachine {
    cores: Vec<CpuState>,
    os: CafeOs,
    registry: RegistryState,
    iosu: Iosu,
    gpu: GpuState,
}

fn state_error(path: &Path, message: impl std::fmt::Display) -> EmuError {
    EmuError::State(format!("{}: {message}", path.display()))
}

/// Where guest memory is not zero, in pieces of at most [`MAX_RUN`] bytes.
fn non_zero_runs(memory: &Memory) -> Result<Vec<(u32, u32)>, EmuError> {
    let mut runs: Vec<(u32, u32)> = Vec::new();
    for region in memory.regions() {
        let bytes = memory.slice(region.start, region.size)?;
        for (index, page) in bytes.chunks(PAGE).enumerate() {
            if page.iter().all(|byte| *byte == 0) {
                continue;
            }
            let address = region.start + (index * PAGE) as u32;
            match runs.last_mut() {
                Some((start, size)) if *start + *size == address && (*size as usize) < MAX_RUN => {
                    *size += page.len() as u32;
                }
                _ => runs.push((address, page.len() as u32)),
            }
        }
    }
    Ok(runs)
}

fn compress_memory(memory: &Memory) -> Result<Vec<MemoryRun>, EmuError> {
    let runs = non_zero_runs(memory)?;
    let next = AtomicUsize::new(0);
    let compress = || {
        let mut done = Vec::new();
        while let Some(&(address, size)) = runs.get(next.fetch_add(1, Ordering::Relaxed)) {
            let bytes = memory.slice(address, size)?;
            let compressed = ByteBuf::from(lz4_flex::compress_prepend_size(bytes));
            done.push(MemoryRun { address, compressed });
        }
        Ok::<_, wiiu_memory::MemoryError>(done)
    };
    let parts: Vec<_> = std::thread::scope(|scope| {
        let workers: Vec<_> = (0..THREADS).map(|_| scope.spawn(compress)).collect();
        workers.into_iter().map(std::thread::ScopedJoinHandle::join).collect()
    });
    let mut memory_runs = Vec::with_capacity(runs.len());
    for part in parts {
        let part = part.map_err(|_| EmuError::State("compressing memory failed".to_owned()))?;
        memory_runs.extend(part?);
    }
    Ok(memory_runs)
}

impl Emulator {
    /// Adds a line to what the title has printed: the window's log shows both.
    #[inline]
    pub fn log_line(&mut self, line: String) {
        self.os.console.push(line);
    }

    /// The file of `slot` for the running title.
    fn state_path(&self, slot: u8) -> Result<PathBuf, EmuError> {
        if slot >= STATE_SLOTS {
            return Err(EmuError::State(format!("no state slot {slot}")));
        }
        if self.status != Status::Running {
            return Err(EmuError::State("no title is running".to_owned()));
        }
        let title = self.title.as_ref().and_then(|title| title.meta.title_id);
        let folder = title.map_or_else(|| "homebrew".to_owned(), |id| format!("{id:016x}"));
        Ok(self.config.states_dir.join(folder).join(format!("{slot}.state")))
    }

    /// First step of saving: the answer holds what [`Self::save_state`] needs, and comes once
    /// [`Self::take_render_commands`] have reached the renderer.
    #[inline]
    pub fn request_surfaces(&mut self) -> std::sync::mpsc::Receiver<Vec<TextureUpload>> {
        self.gpu.request_surfaces()
    }

    /// Saves the machine and the `surfaces` the renderer answered with to `slot`, between frames.
    ///
    /// # Errors
    /// Fails if no title runs or the file cannot be written. An older state in the slot stays.
    pub fn save_state(&self, slot: u8, surfaces: Vec<TextureUpload>) -> Result<PathBuf, EmuError> {
        let path = self.state_path(slot)?;
        let machine = MachineRef {
            cores: &self.cores,
            os: &self.os,
            registry: self.os.registry.save_state(),
            iosu: &self.iosu,
            gpu: self.gpu.save_state(surfaces),
        };
        let machine = postcard::to_stdvec(&machine).map_err(|e| state_error(&path, e))?;
        let file = StateFile {
            format: FORMAT,
            title_id: self.title.as_ref().and_then(|title| title.meta.title_id),
            machine: ByteBuf::from(lz4_flex::compress_prepend_size(&machine)),
            memory: compress_memory(&self.memory)?,
        };
        let bytes = postcard::to_stdvec(&file).map_err(|e| state_error(&path, e))?;
        let io = |source| EmuError::Io { path: path.clone(), source };
        if let Some(folder) = path.parent() {
            std::fs::create_dir_all(folder).map_err(io)?;
        }
        // A crash while writing must not cost the state already there.
        let unfinished = path.with_extension("state.tmp");
        std::fs::write(&unfinished, &bytes).map_err(io)?;
        std::fs::rename(&unfinished, &path).map_err(io)?;
        Ok(path)
    }

    /// Goes on from the state in `slot`, between frames.
    ///
    /// # Errors
    /// Fails if the state cannot be read, or belongs to another title or build. The running
    /// game is left as it was then.
    pub fn load_state(&mut self, slot: u8) -> Result<PathBuf, EmuError> {
        let path = self.state_path(slot)?;
        let bytes =
            std::fs::read(&path).map_err(|source| EmuError::Io { path: path.clone(), source })?;
        let file: StateFile = postcard::from_bytes(&bytes).map_err(|e| state_error(&path, e))?;
        if file.format != FORMAT {
            return Err(state_error(&path, "saved by a different version of shocora"));
        }
        let title_id = self.title.as_ref().and_then(|title| title.meta.title_id);
        if file.title_id != title_id {
            return Err(state_error(&path, "saved by another title"));
        }
        let machine = lz4_flex::decompress_size_prepended(&file.machine)
            .map_err(|e| state_error(&path, e))?;
        let machine: SavedMachine =
            postcard::from_bytes(&machine).map_err(|e| state_error(&path, e))?;
        let mut memory_runs = Vec::with_capacity(file.memory.len());
        for run in &file.memory {
            let bytes = lz4_flex::decompress_size_prepended(&run.compressed)
                .map_err(|e| state_error(&path, e))?;
            if !self.memory.is_mapped(run.address, bytes.len() as u32) {
                return Err(state_error(&path, "memory the running title does not have"));
            }
            memory_runs.push((run.address, bytes));
        }
        self.os.registry.restore_state(machine.registry).map_err(|e| state_error(&path, e))?;

        // The file is good: from here on the machine is replaced.
        let regions: Vec<_> = self.memory.regions().collect();
        for region in regions {
            self.memory.fill(region.start, region.size, 0)?;
        }
        for (address, bytes) in &memory_runs {
            self.memory.write_bytes(*address, bytes)?;
        }
        self.cores = machine.cores;
        self.jit = Jit::new();
        let mut os = machine.os;
        os.registry = std::mem::take(&mut self.os.registry);
        os.gamepad = self.os.gamepad;
        os.pro_controllers = self.os.pro_controllers;
        os.timebase.set_speed(self.speed);
        os.gx2.forced_swap_interval = self.config.swap_interval;
        os.gx2.min_swap_interval = self.config.min_swap_interval.unwrap_or(0);
        os.coreinit.shared_data.fonts_dir.clone_from(&self.config.fonts_dir);
        self.os = os;
        let mut iosu = machine.iosu;
        iosu.adopt_volumes(&mut self.iosu);
        self.iosu = iosu;
        self.gpu.restore_state(machine.gpu, self.config.resolution_factor);
        self.audio_speed = SpeedResampler::default();
        self.next_vsync = None;
        Ok(path)
    }
}
