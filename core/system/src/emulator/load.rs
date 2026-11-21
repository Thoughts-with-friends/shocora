//! Booting titles: mount volumes, load the RPX, set up the main thread.
use std::path::Path;

use wiiu_cafe_os::{BootInfo, CafeOs, aoc::AocTitle};
use wiiu_cpu::CpuState;
use wiiu_fs::{HostDirectory, Overlay, Vfs};
use wiiu_gpu::Gpu;
use wiiu_graphic_pack::{AppliedPack, ReplacedVolume};
use wiiu_iosu::Iosu;
use wiiu_memory::Memory;
use wiiu_title::{
    Dlc, TitleInfo, TitleSource,
    update::{self, AddOnKind},
};

use super::{BootSource, CORE_COUNT, EmuStats, Emulator, MAIN_CORE, Status};
use crate::EmuError;

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
        self.boot(&name, &rpx, Iosu::new(vfs))?;
        self.gpu.set_texture_rules(
            packs.iter().flat_map(|pack| pack.rules.texture_rules.iter().cloned()).collect(),
        );
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
    fn apply_update(&self, info: &mut TitleInfo) -> Result<Vec<String>, EmuError> {
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
        self.boot(name, rpx, Iosu::default())?;
        self.title = None;
        self.boot_source = Some(BootSource::Rpx(name.to_owned(), rpx.to_vec()));
        Ok(())
    }

    /// The enabled graphic packs made for `title_id`, each named in `notices`.
    fn graphic_packs(&self, title_id: Option<u64>, notices: &mut Vec<String>) -> Vec<AppliedPack> {
        let Some(title_id) = title_id else { return Vec::new() };
        let packs = wiiu_graphic_pack::active_for_title(
            &self.config.graphic_pack_dir,
            &self.config.graphic_packs,
            title_id,
        );
        for pack in &packs {
            notices.push(format!("graphic pack: {}", pack.name));
            // Code patches need Cemu's PowerPC assembler, and replaced shaders are GLSL
            // written for Cemu's shader translation: neither applies here.
            if !pack.rules.patch_files.is_empty() {
                notices.push(format!("graphic pack {}: code patches not supported", pack.name));
            }
            if !pack.rules.shaders.is_empty() {
                notices
                    .push(format!("graphic pack {}: shader replacement not supported", pack.name));
            }
        }
        packs
    }

    /// Uses the graphic pack folder and choices `packs`. While a title runs, the packs'
    /// replaced files (mods) take effect at once; their GPU rules wait for the next boot.
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
        self.os.console.extend(notices);
        Ok(())
    }

    fn mount_volumes(&self, info: &TitleInfo, packs: &[AppliedPack]) -> Result<Vfs, EmuError> {
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
    fn mount_replaceable(
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
    fn boot(&mut self, name: &str, rpx: &[u8], iosu: Iosu) -> Result<(), EmuError> {
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
        start_main_thread(&mut self.cores[MAIN_CORE], &boot);

        self.gpu.blank_screens();
        self.status = Status::Running;

        Ok(())
    }
}

/// Content of the system title with the console's language and country lists.
const LANGUAGE_DATA_DIR: &str = "/vol/storage_mlc01/sys/title/0005001b/1005c000/content";

/// Codes of the languages, by the console's language setting.
const LANGUAGES: [&str; 12] =
    ["ja", "en", "fr", "de", "it", "es", "zh", "ko", "nl", "pt", "ru", "zh"];

/// Codes of the countries, by the console's country setting (Cemu's table); the gaps are
/// `NULL`.
const COUNTRIES: &[(usize, &str)] = &[
    (1, "JP"),
    (8, "AI"),
    (9, "AG"),
    (10, "AR"),
    (11, "AW"),
    (12, "BS"),
    (13, "BB"),
    (14, "BZ"),
    (15, "BO"),
    (16, "BR"),
    (17, "VG"),
    (18, "CA"),
    (19, "KY"),
    (20, "CL"),
    (21, "CO"),
    (22, "CR"),
    (23, "DM"),
    (24, "DO"),
    (25, "EC"),
    (26, "SV"),
    (27, "GF"),
    (28, "GD"),
    (29, "GP"),
    (30, "GT"),
    (31, "GY"),
    (32, "HT"),
    (33, "HN"),
    (34, "JM"),
    (35, "MQ"),
    (36, "MX"),
    (37, "MS"),
    (38, "AN"),
    (39, "NI"),
    (40, "PA"),
    (41, "PY"),
    (42, "PE"),
    (43, "KN"),
    (44, "LC"),
    (45, "VC"),
    (46, "SR"),
    (47, "TT"),
    (48, "TC"),
    (49, "US"),
    (50, "UY"),
    (51, "VI"),
    (52, "VE"),
    (64, "AL"),
    (65, "AU"),
    (66, "AT"),
    (67, "BE"),
    (68, "BA"),
    (69, "BW"),
    (70, "BG"),
    (71, "HR"),
    (72, "CY"),
    (73, "CZ"),
    (74, "DK"),
    (75, "EE"),
    (76, "FI"),
    (77, "FR"),
    (78, "DE"),
    (79, "GR"),
    (80, "HU"),
    (81, "IS"),
    (82, "IE"),
    (83, "IT"),
    (84, "LV"),
    (85, "LS"),
    (86, "LI"),
    (87, "LT"),
    (88, "LU"),
    (89, "MK"),
    (90, "MT"),
    (91, "ME"),
    (92, "MZ"),
    (93, "NA"),
    (94, "NL"),
    (95, "NZ"),
    (96, "NO"),
    (97, "PL"),
    (98, "PT"),
    (99, "RO"),
    (100, "RU"),
    (101, "RS"),
    (102, "SK"),
    (103, "SI"),
    (104, "ZA"),
    (105, "ES"),
    (106, "SZ"),
    (107, "SE"),
    (108, "CH"),
    (109, "TR"),
    (110, "GB"),
    (111, "ZM"),
    (112, "ZW"),
    (113, "AZ"),
    (114, "MR"),
    (115, "ML"),
    (116, "NE"),
    (117, "TD"),
    (118, "SD"),
    (119, "ER"),
    (120, "DJ"),
    (121, "SO"),
    (122, "AD"),
    (123, "GI"),
    (124, "GG"),
    (125, "IM"),
    (126, "JE"),
    (127, "MC"),
    (128, "TW"),
    (136, "KR"),
    (144, "HK"),
    (145, "MO"),
    (152, "ID"),
    (153, "SG"),
    (154, "TH"),
    (155, "PH"),
    (156, "MY"),
    (160, "CN"),
    (168, "AE"),
    (170, "EG"),
    (171, "OM"),
    (172, "QA"),
    (173, "KW"),
    (174, "SA"),
    (175, "SY"),
    (176, "BH"),
    (177, "JO"),
    (184, "SM"),
    (185, "VA"),
    (186, "BM"),
    (187, "IN"),
    (192, "NG"),
    (193, "AO"),
    (194, "GH"),
];

/// `language.txt` and `country.txt` of the language data title, as Cemu makes them: one
/// quoted code and a comma per line.
fn language_data() -> Result<Overlay, EmuError> {
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
fn start_main_thread(cpu: &mut CpuState, boot: &BootInfo) {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Config;

    #[test]
    fn rejects_garbage_rpx() {
        let mut emu = Emulator::new(Config::default()).unwrap();
        assert!(emu.load_rpx("bad.rpx", b"not an elf").is_err());
        assert_eq!(*emu.status(), Status::Idle);
    }
}
