//! Part of [`super`]: see its documentation.
//!
//! ## Where this file sits
//!
//! ```text
//! Game files ─► Loader ─► CPU ◄─► Memory ◄─► OS (HLE) ─► GPU ─► Screen
//! ▲▲▲▲▲▲▲▲▲▲    ▲▲▲▲▲▲  here
//! Controllers, clock ─► OS (HLE) ─► Sound ─► Speakers
//!
//! ┌────────────────────┐   ┌──────────────────────────┐   ┌────────────────────┐
//! │ game folder        │   │ emulator/load/boot_part. │   │ running main       │
//! │                    │   │ rs                       │   │ thread             │
//! │                    │──►│ ──────────────────────── │──►│                    │
//! │                    │   │ booting a game: boot     │   │                    │
//! │                    │   │ part                     │   │                    │
//! └────────────────────┘   └──────────────────────────┘   └────────────────────┘
//! ```
//!
//! The "boot part" share of load.rs (booting a game), moved here so that no file is longer than
//! 500 lines. It works exactly as if it were still written in that file. Mounts the game files,
//! loads the program and starts it. Terms such as HLE, JIT or shader are explained in
//! `docs/guide/wiiu_term.md`.

use super::*;

impl Emulator {
    /// Boots a title directory or `.rpx` file.
    ///
    /// # Errors
    /// Fails if the path is not a bootable title or the RPX does not load.
    pub fn load_title(&mut self, path: &Path) -> Result<(), EmuError> {
        let mut info = TitleSource::open(path)?;
        let mut notices = self.apply_update(&mut info)?;
        let packs = self.graphic_packs(info.meta.title_id, &mut notices);

        // The RPX is the title's entry point, and the volumes are its file system.
        let rpx = std::fs::read(&info.rpx_path)
            .map_err(|source| EmuError::Io { path: info.rpx_path.clone(), source })?;

        // The volumes are mounted read-only, except for `/vol/save` which is writable.
        let vfs = self.mount_volumes(&info, &packs)?;

        // The RPX is the title's entry point, and the volumes are its file system.
        let name = info
            .rpx_path
            .file_name()
            .map_or_else(|| "main.rpx".into(), |n| n.to_string_lossy().into_owned());

        // The title's RPX is loaded into memory and the main thread is started.
        self.boot(&name, &rpx, Iosu::new(vfs), &packs, &mut notices)?;
        self.gpu.set_texture_rules(
            packs.iter().flat_map(|pack| pack.rules.texture_rules.iter().cloned()).collect(),
        );
        self.pack_frame_time = pack_frame_time(&packs, &mut notices);
        // Cemu takes the output settings of the last pack with an `[OutputShader]`.
        self.output_nearest = packs
            .iter()
            .rev()
            .map(|pack| super::super::OutputFilter {
                upscale_nearest: pack.rules.upscale_nearest,
                downscale_nearest: pack.rules.downscale_nearest,
            })
            .find(|filter| *filter != super::super::OutputFilter::default())
            .unwrap_or_default();
        self.os.title_id = info.meta.title_id.unwrap_or(0);
        self.os.aoc = info
            .dlc
            .iter()
            .map(|dlc| AocTitle { title_id: dlc.title_id, version: dlc.version })
            .collect();

        // Shown in the window's log: the first thing to look at when a title will not start.
        self.os.console.extend(notices);
        self.title = Some(info);
        self.boot_source = Some(BootSource::Title(path.to_owned()));

        if let Err(error) = self.reload_cheats() {
            log::warn!("{error}");
        }

        Ok(())
    }

    /// Finds the title's update and lays it over the title, and finds its downloadable
    /// content. Returns what the log should say: what is used, and which folders were passed
    /// over and why.
    pub(super) fn apply_update(&self, info: &mut TitleInfo) -> Result<Vec<String>, EmuError> {
        let Some(title_id) = info.meta.title_id else { return Ok(Vec::new()) };

        let root = match &info.source {
            TitleSource::Directory(root) => Some(root.clone()),
            TitleSource::Rpx(_) => None,
        };

        let config = &self.config;
        let find = |kind, libraries: &[std::path::PathBuf]| {
            update::find_add_on(kind, title_id, root.as_deref(), libraries, &config.mlc_dirs)
        };

        let updates = find(AddOnKind::Update, &config.update_dirs);
        let dlc = find(AddOnKind::Dlc, &config.dlc_dirs);

        let mut notices: Vec<String> = updates
            .rejected
            .iter()
            .chain(&dlc.rejected)
            .map(|(dir, problem)| format!("[shocora] ignored: {}: {problem}", dir.display()))
            .collect();

        if let Some(dir) = updates.found {
            let base = info.clone();
            *info = base.with_update(&dir)?;
            let version = info.meta.title_version.unwrap_or(0);
            notices.push(format!("[shocora] update v{version}: {}", dir.display()));
        }

        if let Some(dir) = dlc.found {
            let version = dlc.version.unwrap_or(0);
            notices.push(format!("[shocora] DLC v{version}: {}", dir.display()));
            info.dlc = Some(Dlc {
                dir,
                title_id: AddOnKind::Dlc.title_id(title_id),
                version: u16::try_from(version).unwrap_or(u16::MAX),
            });
        }

        for notice in &notices {
            log::info!("{notice}");
        }

        Ok(notices)
    }

    /// Reboots what was booted last, like the console's reset; `false` if nothing was.
    ///
    /// # Errors
    /// Fails if the title does not load any more.
    pub fn reset(&mut self) -> Result<bool, EmuError> {
        match self.boot_source.clone() {
            Some(BootSource::Title(path)) => self.load_title(&path)?,
            Some(BootSource::Rpx(name, rpx)) => self.load_rpx(&name, &rpx)?,
            None => return Ok(false),
        }
        Ok(true)
    }

    /// Boots an in-memory RPX with no file system (tests, tools).
    ///
    /// # Errors
    /// Fails if the RPX does not load.
    pub fn load_rpx(&mut self, name: &str, rpx: &[u8]) -> Result<(), EmuError> {
        self.boot(name, rpx, Iosu::default(), &[], &mut Vec::new())?;
        self.title = None;
        self.pack_frame_time = None;
        self.output_nearest = super::super::OutputFilter::default();
        self.boot_source = Some(BootSource::Rpx(name.to_owned(), rpx.to_vec()));
        Ok(())
    }

    /// The enabled graphic packs made for `title_id`, each named in `notices`.
    pub(super) fn graphic_packs(
        &self,
        title_id: Option<u64>,
        notices: &mut Vec<String>,
    ) -> Vec<AppliedPack> {
        let Some(title_id) = title_id else { return Vec::new() };
        let packs = wiiu_graphic_pack::active_for_title(
            &self.config.graphic_pack_dir,
            &self.config.graphic_packs,
            title_id,
        );
        for pack in &packs {
            notices.push(format!("graphic pack: {}", pack.name));
            // Replaced shaders are GLSL written for Cemu's shader translation.
            if !pack.rules.shaders.is_empty() {
                notices
                    .push(format!("graphic pack {}: shader replacement not supported", pack.name));
            }
        }
        packs
    }

    /// Uses the graphic pack folder and choices `packs`. While a title runs, the packs'
    /// replaced files and code patches (mods) take effect at once; their GPU rules wait
    /// for the next boot.
    ///
    /// # Errors
    /// Fails if the volumes cannot be mounted again.
    pub fn set_graphic_packs(
        &mut self,
        dir: std::path::PathBuf,
        packs: wiiu_graphic_pack::PackSettings,
    ) -> Result<(), EmuError> {
        self.config.graphic_pack_dir = dir;
        self.config.graphic_packs = packs;
        let Some(info) = &self.title else { return Ok(()) };
        let mut notices = Vec::new();
        let applied = self.graphic_packs(info.meta.title_id, &mut notices);
        let mut mounts = Vec::new();
        self.mount_replaceable(info, &applied, &mut |point, device| {
            mounts.push((point.to_owned(), device));
            Ok(())
        })?;
        for (point, device) in mounts {
            self.iosu.vfs_mut().mount(&point, device)?;
        }
        self.apply_patches(&applied, &mut notices, false);
        self.os.console.extend(notices);
        Ok(())
    }

    pub(super) fn mount_volumes(
        &self,
        info: &TitleInfo,
        packs: &[AppliedPack],
    ) -> Result<Vfs, EmuError> {
        let mut vfs = Vfs::new();
        self.mount_replaceable(info, packs, &mut |point, device| vfs.mount(point, device))?;

        if let Some(meta) = &info.meta_dir {
            let update =
                info.update_dir.as_ref().map(|dir| dir.join("meta")).filter(|d| d.is_dir());
            let meta = update
                .into_iter()
                .fold(Overlay::new(), |overlay, dir| overlay.layer(HostDirectory::read_only(dir)))
                .layer(HostDirectory::read_only(meta));
            vfs.mount("/vol/meta", meta)?;
        }

        let title_id =
            info.meta.title_id.map_or_else(|| "homebrew".to_owned(), |id| format!("{id:016x}"));

        // The system title holding the language and country lists: titles read them to name
        // files after the console's settings (Paper Mario: Color Splash fails to boot without).
        vfs.mount(LANGUAGE_DATA_DIR, language_data()?)?;

        // The title's save data is writable, and is stored in the host's `save` folder.
        vfs.mount("/vol/save", HostDirectory::writable(self.config.save_dir.join(title_id)))?;

        Ok(vfs)
    }

    /// Mounts the volumes graphic packs can replace files of: code, content and the DLC's.
    pub(super) fn mount_replaceable(
        &self,
        info: &TitleInfo,
        packs: &[AppliedPack],
        mount: &mut dyn FnMut(&str, Overlay) -> Result<(), wiiu_fs::FsError>,
    ) -> Result<(), EmuError> {
        // The graphic packs' files lie over an installed update's, which lie over the
        // title's; the first pack has the highest priority.
        let replaced = |volume: ReplacedVolume| {
            packs.iter().map(move |pack| pack.replacement_dir(volume)).filter(|dir| dir.is_dir())
        };
        let volume = |base: &Path, name: &str, replacing: ReplacedVolume| {
            let update = info.update_dir.as_ref().map(|dir| dir.join(name)).filter(|d| d.is_dir());
            replaced(replacing)
                .chain(update)
                .fold(Overlay::new(), |overlay, dir| overlay.layer(HostDirectory::read_only(dir)))
                .layer(HostDirectory::read_only(base))
        };

        mount("/vol/code", volume(&info.code_dir, "code", ReplacedVolume::Code))?;

        if let Some(content) = &info.content_dir {
            let mut content = volume(content, "content", ReplacedVolume::Content);
            let gate = info.meta.title_id.and_then(update::version_gate);
            if let Some(gate) =
                gate.filter(|_| info.update_dir.is_none() && self.config.skip_update_check)
            {
                log::info!("no update installed: skipping the title's update check");
                content = content.file(gate.file, gate.contents)?;
            }
            mount("/vol/content", content)?;
        }

        if let Some(dlc) = &info.dlc {
            let aoc = AocTitle { title_id: dlc.title_id, version: dlc.version };
            let content = replaced(ReplacedVolume::Aoc)
                .fold(Overlay::new(), |overlay, dir| overlay.layer(HostDirectory::read_only(dir)))
                .layer(HostDirectory::read_only(dlc.dir.join("content")));
            mount(&aoc.path(), content)?;
        }
        Ok(())
    }

    /// Resets the machine and starts `rpx` on the main core.
    pub(super) fn boot(
        &mut self,
        name: &str,
        rpx: &[u8],
        iosu: Iosu,
        packs: &[AppliedPack],
        notices: &mut Vec<String>,
    ) -> Result<(), EmuError> {
        let gamepad = self.os.gamepad;
        let pro_controllers = self.os.pro_controllers;

        self.memory = Memory::with_game_layout()?;
        self.cores = (0..CORE_COUNT as u32).map(CpuState::new).collect();
        self.jit = wiiu_cpu::Jit::new();

        self.os = CafeOs::new();
        self.os.timebase.set_speed(self.speed);
        self.os.gamepad = gamepad;
        self.os.pro_controllers = pro_controllers;
        self.os.gx2.forced_swap_interval = self.config.swap_interval;
        self.os.gx2.min_swap_interval = self.config.min_swap_interval.unwrap_or(0);
        self.os.coreinit.shared_data.fonts_dir.clone_from(&self.config.fonts_dir);

        self.gpu = if self.config.gpu_thread { Gpu::threaded() } else { Gpu::new() };
        self.gpu.reset_renderer();
        self.gpu.set_resolution_factor(self.config.resolution_factor);

        // IOSU means "I/O and system utilities":
        // the Wii U's OS, which is not the same as the host's.
        self.iosu = iosu;

        self.stats = EmuStats::default();
        self.next_vsync = None;
        self.multi_core_running = false;

        let boot = self.os.boot(&mut self.memory, name, rpx)?;
        self.patches = super::super::patches::PatchState::for_module(&boot.module);
        self.apply_patches(packs, notices, true);
        start_main_thread(&mut self.cores[MAIN_CORE], &boot);

        self.gpu.blank_screens();
        self.status = Status::Running;

        Ok(())
    }
}

/// `language.txt` and `country.txt` of the language data title, as Cemu makes them: one
/// quoted code and a comma per line.
/// The vertical blank period of the first pack setting `vsyncFrequency`
/// (Cemu's `LatteTiming_setCustomVsyncFrequency`). Packs asking for another frequency are
/// named in `notices` and ignored, as in Cemu.
pub(super) fn pack_frame_time(
    packs: &[AppliedPack],
    notices: &mut Vec<String>,
) -> Option<std::time::Duration> {
    let mut chosen: Option<u32> = None;
    for pack in packs {
        let Some(hz) = pack.rules.vsync_frequency else { continue };
        match chosen {
            None => {
                chosen = Some(hz);
                notices.push(format!("graphic pack {}: vsync frequency {hz} Hz", pack.name));
            }
            Some(first) if first != hz => notices.push(format!(
                "graphic pack {}: vsync frequency {hz} Hz ignored, {first} Hz is set",
                pack.name
            )),
            Some(_) => {}
        }
    }
    chosen.map(|hz| std::time::Duration::from_secs_f64(1.0 / f64::from(hz)))
}

pub(super) fn language_data() -> Result<Overlay, EmuError> {
    let languages: String = LANGUAGES.iter().map(|code| format!("\"{code}\",\n")).collect();
    let last = COUNTRIES.last().map_or(0, |(index, _)| *index);
    let countries: String = (0..=last)
        .map(|index| match COUNTRIES.iter().find(|(i, _)| *i == index) {
            Some((_, code)) => format!("\"{code}\",\n"),
            None => "NULL,\n".to_owned(),
        })
        .collect();
    Ok(Overlay::new().file("/language.txt", languages)?.file("/country.txt", countries)?)
}

/// Register state at the entry point (`__preinit_user` or the title entry).
pub(super) fn start_main_thread(cpu: &mut CpuState, boot: &BootInfo) {
    cpu.pc = boot.entry;
    cpu.lr = boot.return_address;

    cpu.gpr[1] = boot.stack_pointer;
    cpu.gpr[2] = boot.sda2_base;
    cpu.gpr[13] = boot.sda_base;
    cpu.gpr[3] = boot.args[0];
    cpu.gpr[4] = boot.args[1];
    cpu.gpr[5] = boot.args[2];

    // Paired-single quantisation presets coreinit installs (UGQR2..=5).
    cpu.gqr[2] = 0x0004_0004;
    cpu.gqr[3] = 0x0005_0005;
    cpu.gqr[4] = 0x0006_0006;
    cpu.gqr[5] = 0x0007_0007;
}
