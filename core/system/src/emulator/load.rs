//! Booting titles: mount volumes, load the RPX, set up the main thread.
//!
//! ## Where this file sits
//!
//! ```text
//! Game files ─► Loader ─► CPU ◄─► Memory ◄─► OS (HLE) ─► GPU ─► Screen
//! ▲▲▲▲▲▲▲▲▲▲    ▲▲▲▲▲▲  here
//! Controllers, clock ─► OS (HLE) ─► Sound ─► Speakers
//!
//! ┌────────────────────┐   ┌──────────────────────────┐   ┌────────────────────┐
//! │ game folder        │   │ emulator/load.rs         │   │ running main       │
//! │                    │──►│ ──────────────────────── │──►│ thread             │
//! │                    │   │ booting a game           │   │                    │
//! └────────────────────┘   └──────────────────────────┘   └────────────────────┘
//! ```
//!
//! Mounts the game files, loads the program and starts it. Terms such as HLE, JIT or shader are
//! explained in `docs/guide/wiiu_term.md`.
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

#[cfg(test)]
#[path = "load_tests.rs"]
mod tests;

mod boot_part;
#[allow(unused_imports)]
pub(crate) use boot_part::*;
