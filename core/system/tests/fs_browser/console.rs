//! The whole console as one tree, the way IOSU (the OS on the Starbuck co-processor)
//! names things, like a Linux root file system:
//!
//! - `/vol/…`: the title's volumes as mounted (`code`, `content`, `meta`, `save`, `aoc…`),
//!   and the same folders at their place on the system memory, under
//!   `/vol/storage_mlc01/usr/title/<high>/<low>/` and `/vol/storage_mlc01/usr/save/…/user`.
//! - `/dev/…`: IOSU's devices Cafe OS talks to over IPC, with their open handles.
//! - `/proc/…`: the inside of the emulator's VFS and IOSU, as text, like Linux's `/proc`.
//!
//! Everything is read-only, and generated again for each request.
use std::{collections::BTreeMap, fmt::Write as _};

use wiiu_emu::Emulator;
use wiiu_fs::Vfs;
use wiiu_iosu::{DeviceKind, Iosu};

/// What one request may see.
pub struct View<'a> {
    pub vfs: &'a Vfs,
    /// Console path → VFS path, longest console path first.
    links: Vec<(String, String)>,
    /// Generated text files, by path.
    texts: BTreeMap<String, String>,
}

/// IOSU's devices: path, kind, what Cafe OS uses it for.
const DEVICES: [(&str, DeviceKind, &str); 4] = [
    ("/dev/fsa", DeviceKind::Fsa, "File system access: every FSOpenFile, FSReadFile, FSGetStat…"),
    (
        "/dev/mcp",
        DeviceKind::Mcp,
        "Title and system management: installed titles, system settings.",
    ),
    ("/dev/acp_main", DeviceKind::Acp, "Application control: save folders, title metadata."),
    ("/dev/act", DeviceKind::Act, "Accounts: the Mii and the user's account."),
];

impl<'a> View<'a> {
    /// `vfs` as it is, each mount at its own point.
    pub fn plain(vfs: &'a Vfs) -> Self {
        let mut view = Self { vfs, links: Vec::new(), texts: BTreeMap::new() };
        for point in vfs.mount_points() {
            view.link(point, point);
        }
        view
    }

    /// The running emulator's console: volumes, system-memory paths, devices and state.
    pub fn console(emu: &'a Emulator) -> Self {
        let iosu = emu.iosu();
        let mut view = Self::plain(iosu.vfs());
        if let Some(id) = emu.title().and_then(|title| title.meta.title_id) {
            let title = title_dir(id);
            for volume in ["code", "content", "meta"] {
                if view.vfs.is_mounted(&format!("/vol/{volume}")) {
                    view.link(&format!("{title}/{volume}"), &format!("/vol/{volume}"));
                }
            }
            let save = format!(
                "/vol/storage_mlc01/usr/save/{:08x}/{:08x}/user",
                id >> 32,
                id & 0xFFFF_FFFF
            );
            view.link(&save, "/vol/save");
        }
        for point in view.vfs.mount_points() {
            if let Some(aoc) =
                point.strip_prefix("/vol/aoc").and_then(|id| u64::from_str_radix(id, 16).ok())
            {
                view.link(&format!("{}/content", title_dir(aoc)), point);
            }
        }
        view.links.sort_by_key(|(console, _)| std::cmp::Reverse(console.len()));
        view.add_state(emu, iosu);
        view
    }

    fn link(&mut self, console: &str, vfs: &str) {
        self.links.push((console.to_owned(), vfs.to_owned()));
    }

    /// `/dev` and `/proc`.
    fn add_state(&mut self, emu: &Emulator, iosu: &Iosu) {
        let handles = iosu.handles();
        for (path, kind, purpose) in DEVICES {
            let open: Vec<String> = handles
                .iter()
                .filter(|(_, k)| *k == kind)
                .map(|(h, _)| format!("{:#x}", h.0))
                .collect();
            let open = if open.is_empty() { "none".to_owned() } else { open.join(", ") };
            self.texts.insert(path.to_owned(), format!("{purpose}\n\nopen handles: {open}\n"));
        }

        let mut mounts = String::from("# mount point -> device\n");
        for (point, device) in self.vfs.mounts() {
            let _ = writeln!(mounts, "{point:<40} {device}");
        }
        self.texts.insert("/proc/vfs/mounts".to_owned(), mounts);

        let mut devices = String::from("# handle  device\n");
        for (handle, kind) in &handles {
            let _ = writeln!(devices, "{:#010x}  {kind:?}", handle.0);
        }
        self.texts.insert("/proc/iosu/handles".to_owned(), devices);

        let mut files = String::from("# handle  position / size  access  path\n");
        for (handle, file) in iosu.fsa.open_files() {
            let access = if file.writable { "rw" } else { "r " };
            let _ = writeln!(
                files,
                "{handle:#010x}  {:>10} / {:<10}  {access}  {}",
                file.position, file.size, file.path
            );
        }
        self.texts.insert("/proc/iosu/fsa/files".to_owned(), files);

        let mut dirs = String::from("# handle  read / entries  first entries\n");
        for (handle, dir) in iosu.fsa.open_dirs() {
            let names: Vec<&str> =
                dir.entries.iter().take(8).map(|(name, _)| name.as_str()).collect();
            let _ = writeln!(
                dirs,
                "{handle:#010x}  {:>4} / {:<4}  {}",
                dir.next,
                dir.entries.len(),
                names.join(", ")
            );
        }
        self.texts.insert("/proc/iosu/fsa/dirs".to_owned(), dirs);

        if let Some(title) = emu.title() {
            let mut text = format!("name: {}\n", title.display_name());
            if let Some(id) = title.meta.title_id {
                let _ = writeln!(text, "title id: {id:016x}");
            }
            let _ = writeln!(text, "version: {:?}", title.meta.title_version);
            let _ = writeln!(text, "rpx: {}", title.rpx_path.display());
            let _ = writeln!(text, "update: {:?}", title.update_dir);
            let _ = writeln!(text, "dlc: {:?}", title.dlc.as_ref().map(|dlc| &dlc.dir));
            self.texts.insert("/proc/title".to_owned(), text);
        }
    }

    /// The VFS path a console path stands for.
    pub fn resolve(&self, path: &str) -> Option<String> {
        self.links.iter().find_map(|(console, vfs)| {
            let rest = path.strip_prefix(console.as_str())?;
            (rest.is_empty() || rest.starts_with('/')).then(|| format!("{vfs}{rest}"))
        })
    }

    /// A generated text file.
    pub fn text(&self, path: &str) -> Option<&str> {
        self.texts.get(path).map(String::as_str)
    }

    /// The generated entries right below `path`: parents of links, `/dev`, `/proc`.
    pub fn children(&self, path: &str) -> Vec<(String, bool, u64)> {
        let prefix = if path.ends_with('/') { path.to_owned() } else { format!("{path}/") };
        let mut entries: BTreeMap<String, (bool, u64)> = BTreeMap::new();
        for console in self.links.iter().map(|(console, _)| console) {
            if let Some(name) =
                console.strip_prefix(&prefix).and_then(|rest| rest.split('/').next())
            {
                entries.insert(name.to_owned(), (true, 0));
            }
        }
        for (file, text) in &self.texts {
            let Some(rest) = file.strip_prefix(&prefix) else { continue };
            match rest.split_once('/') {
                Some((dir, _)) => entries.insert(dir.to_owned(), (true, 0)),
                None => entries.insert(rest.to_owned(), (false, text.len() as u64)),
            };
        }
        entries
            .into_iter()
            .filter(|(name, _)| !name.is_empty())
            .map(|(n, (d, s))| (n, d, s))
            .collect()
    }

    /// Whether `path` is a generated folder.
    pub fn is_virtual_dir(&self, path: &str) -> bool {
        path == "/" || !self.children(path).is_empty()
    }
}

/// `/vol/storage_mlc01/usr/title/<high>/<low>`: where a title is installed.
fn title_dir(id: u64) -> String {
    format!("/vol/storage_mlc01/usr/title/{:08x}/{:08x}", id >> 32, id & 0xFFFF_FFFF)
}
