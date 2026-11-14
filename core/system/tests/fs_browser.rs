//! A browser view of the file system the emulated title sees, served over HTTP.
//!
//! `cargo test -p wiiu_emu --test fs_browser` checks the routes. To look around, run the
//! ignored test [`serve_file_browser`] (rust-analyzer's "Run Test" above it, or
//! `cargo test -p wiiu_emu --test fs_browser serve_file_browser -- --ignored --nocapture`).
//! It boots the title of `shocora.toml` and keeps serving while it runs. `SHOCORA_FS_BROWSER`
//! picks another port than 8080.
//!
//! Then open <http://127.0.0.1:8080>: an explorer of `/vol/…` on the left, files in tabs
//! (text with line numbers, anything else as hex). The tree is the console's, as IOSU
//! names it: `/vol` (with `storage_mlc01`), `/dev` and `/proc` (see `console.rs`). Stop it with Ctrl+C. The server listens
//! on the loopback address only, answers `GET` only, and the save is a temporary copy.
#![allow(clippy::unwrap_used, reason = "test")]
#[path = "fs_browser/console.rs"]
mod console;
#[path = "fs_browser/server.rs"]
mod server;

use std::{
    io::{ErrorKind, Read, Write},
    net::{TcpListener, TcpStream},
    path::Path,
    time::{Duration, Instant},
};

use console::View;
use server::{json_string, respond};
use wiiu_emu::{Config, Emulator, Status};
use wiiu_fs::{HostDirectory, Vfs};
use wiiu_test_roms::{report, require};

/// How long a connection may stay open without sending a request.
const IDLE: Duration = Duration::from_secs(10);

/// Frames run before the file system is checked: one second at 60 Hz.
const FRAMES: u32 = 60;

/// A guest file system of a temporary folder mounted at `/vol/save`.
fn sample_vfs(tag: &str) -> (Vfs, std::path::PathBuf) {
    let dir = std::env::temp_dir().join(format!("shocora-fs-browser-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("0")).unwrap();
    std::fs::write(dir.join("0/option.sav"), [0_u8, 1, 2, 3, 4, 5]).unwrap();
    std::fs::write(dir.join("readme.txt"), "line 1\nline 2\n").unwrap();
    let mut vfs = Vfs::new();
    vfs.mount("/vol/save", HostDirectory::read_only(&dir)).unwrap();
    (vfs, dir)
}

/// Intent: the routes the page uses: the root lists the parents of mount points, a
/// folder lists its device's entries, a file comes back in slices with its full size.
/// Pass: `/` holds `vol`, `/vol` holds `save`, `/vol/save` lists the folder `0` before
/// `readme.txt`; bytes 2..5 of `option.sav` are `2 3 4` with size 6; a host-style path
/// (`\`, `:`) is refused with 400 and an unknown folder is 404.
#[test]
fn routes_list_folders_and_slice_files() {
    let (vfs, dir) = sample_vfs("routes");
    let vfs = View::plain(&vfs);
    assert!(respond(&vfs, "{}", "/").text().contains("EXPLORER"));
    assert!(respond(&vfs, "{}", "/index.js").text().contains("function openFile"));
    assert!(respond(&vfs, "{}", "/api/dir?path=%2F").text().contains(r#""name":"vol""#));
    assert!(respond(&vfs, "{}", "/api/dir?path=/vol").text().contains(r#""name":"save""#));

    let save = respond(&vfs, "{}", "/api/dir?path=/vol/save").text();
    let folder = save.find(r#""name":"0""#).unwrap();
    assert!(folder < save.find("readme.txt").unwrap(), "{save}");

    let slice = respond(&vfs, "{}", "/api/file?path=/vol/save/0/option.sav&offset=2&len=3");
    assert_eq!((slice.status, slice.size, slice.body), (200, Some(6), vec![2, 3, 4]));

    assert_eq!(respond(&vfs, "{}", "/api/file?path=/vol/save/..%5C..%5Cx").status, 400);
    assert_eq!(respond(&vfs, "{}", "/api/dir?path=/vol/none").status, 404);
    let _ = std::fs::remove_dir_all(dir);
}

/// Intent: the page and the routes travel over a real socket, as a browser sees them.
/// Pass: a `GET` of `/vol/save/readme.txt` through a loopback connection answers 200
/// with `X-Size: 14` and the file's text; a `POST` is refused with 405.
#[test]
fn serves_over_http() {
    let (vfs, dir) = sample_vfs("http");
    let vfs = View::plain(&vfs);
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let ask = move |request: &'static str| {
        std::thread::spawn(move || {
            let mut stream = TcpStream::connect(address).unwrap();
            stream.write_all(request.as_bytes()).unwrap();
            let mut answer = String::new();
            stream.read_to_string(&mut answer).unwrap();
            answer
        })
    };

    let client = ask("GET /api/file?path=/vol/save/readme.txt HTTP/1.1\r\nHost: x\r\n\r\n");
    server::serve(listener.accept().unwrap().0, &vfs, "{}").unwrap();
    let answer = client.join().unwrap();
    assert!(answer.starts_with("HTTP/1.1 200"), "{answer}");
    assert!(answer.contains("X-Size: 14") && answer.ends_with("line 1\nline 2\n"));

    let client = ask("POST / HTTP/1.1\r\n\r\n");
    server::serve(listener.accept().unwrap().0, &vfs, "{}").unwrap();
    assert!(client.join().unwrap().starts_with("HTTP/1.1 405"));
    let _ = std::fs::remove_dir_all(dir);
}

/// What `/api/status` says about the running emulator.
fn status_json(emu: &Emulator, title: &Path) -> String {
    let stats = emu.stats();
    let name = title.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    format!(
        r#"{{"title":{},"status":{},"frames":{},"hle_calls":{}}}"#,
        json_string(&name),
        json_string(&format!("{:?}", emu.status())),
        stats.frames,
        stats.hle_calls
    )
}

/// Intent: the file system the emulator mounted for the running title (with its update
/// laid over it and the save mounted writable), seen through the browser's routes.
/// Pass: after a second of running, `/` holds `dev`, `proc` and `vol`; `/vol` lists
/// `code`, `content`, `meta`, `save` and `storage_mlc01`; `/vol/code` holds the `.rpx`;
/// `/vol/code/app.xml` is byte for byte what the emulator reads, and so is the same file
/// at `/vol/storage_mlc01/usr/title/<high>/<low>/code`; `/proc/vfs/mounts` lists the
/// content mount and its host folder; `/dev/fsa` says which handles are open.
#[test]
fn browses_the_running_titles_file_system() {
    let title = require!(wiiu_test_roms::title(), "no title");
    let (mut emu, save_dir) = boot(&title, "fs-browser");

    let view = View::console(&emu);
    let ask = |target: &str| respond(&view, "{}", target).text();
    let volumes = ask("/api/dir?path=/vol");
    for volume in ["code", "content", "meta", "save", "storage_mlc01"] {
        assert!(volumes.contains(&format!(r#""name":"{volume}""#)), "{volumes}");
    }
    assert!(ask("/api/dir?path=/vol/code").contains(".rpx"));
    let app = respond(&view, "{}", "/api/file?path=/vol/code/app.xml");
    assert_eq!(app.body, emu.vfs().read("/vol/code/app.xml").unwrap());

    // The same folder where the system memory keeps it.
    let id = wiiu_test_roms::title_id(&title).unwrap();
    let installed =
        format!("/vol/storage_mlc01/usr/title/{:08x}/{:08x}", id >> 32, id & 0xFFFF_FFFF);
    let app_again = respond(&view, "{}", &format!("/api/file?path={installed}/code/app.xml"));
    assert_eq!(app_again.body, app.body);

    let root = ask("/api/dir?path=/");
    assert!(
        ["dev", "proc", "vol"].iter().all(|d| root.contains(&format!(r#""name":"{d}""#))),
        "{root}"
    );
    let mounts = ask("/api/file?path=/proc/vfs/mounts");
    assert!(mounts.contains("/vol/content") && mounts.contains("host folder"), "{mounts}");
    let handles = ask("/api/file?path=/proc/iosu/handles");
    assert!(ask("/api/file?path=/dev/fsa").contains("open handles"));

    report("/", root);
    report("/vol", volumes);
    report("installed at", installed);
    for line in mounts.lines().chain(handles.lines()) {
        report("  /proc", line);
    }
    report("/proc/iosu/fsa/files", ask("/api/file?path=/proc/iosu/fsa/files").lines().count() - 1);
    emu.stop();
    let _ = std::fs::remove_dir_all(save_dir);
}

/// Boots `title` with a temporary copy of its save and runs it for [`FRAMES`].
fn boot(title: &Path, tag: &str) -> (Emulator, std::path::PathBuf) {
    let save_dir = wiiu_test_roms::temp_save_dir(title, tag);
    let config = Config { save_dir: save_dir.clone(), ..Config::default() };
    let mut emu = Emulator::new(config).unwrap();
    emu.load_title(title).unwrap();
    for _ in 0..FRAMES {
        emu.run_frame();
    }
    (emu, save_dir)
}

/// Not a check: the browser itself. Boots the title of `shocora.toml` and serves
/// <http://127.0.0.1:8080> (or the port in `SHOCORA_FS_BROWSER`) while it runs, until the
/// test is stopped. Ignored by `cargo test`; rust-analyzer's "Run Test" runs it.
#[test]
#[ignore = "serves until stopped: run it on its own"]
fn serve_file_browser() {
    let title = require!(wiiu_test_roms::title(), "no title");
    let port = std::env::var("SHOCORA_FS_BROWSER").unwrap_or_else(|_| "8080".to_owned());
    let (mut emu, _save_dir) = boot(&title, "fs-browser-serve");
    serve_while_running(&mut emu, &title, &port);
}

/// Runs the title and answers the browser between frames, until the process is stopped.
fn serve_while_running(emu: &mut Emulator, title: &Path, port: &str) {
    let listener = TcpListener::bind(format!("127.0.0.1:{port}")).unwrap();
    listener.set_nonblocking(true).unwrap();
    println!("guest file system: http://127.0.0.1:{port}  (Ctrl+C to stop)");
    // Browsers open spare connections that may never carry a request: they wait here,
    // without holding up the emulator, until their request has arrived.
    let mut pending: Vec<(TcpStream, Instant)> = Vec::new();
    loop {
        if *emu.status() == Status::Running {
            emu.run_frame();
        } else {
            std::thread::sleep(Duration::from_millis(16));
        }
        while let Ok((stream, _)) = listener.accept() {
            if stream.set_nonblocking(true).is_ok() {
                pending.push((stream, Instant::now()));
            }
        }
        if pending.is_empty() {
            continue;
        }
        let status = status_json(emu, title);
        let view = View::console(emu);
        pending.retain(|(stream, since)| {
            let mut head = [0_u8; 8192];
            match stream.peek(&mut head) {
                // Closed before asking anything.
                Ok(0) => false,
                Ok(n) if n == head.len() || head[..n].windows(4).any(|w| w == b"\r\n\r\n") => {
                    let answered =
                        stream.try_clone().and_then(|s| server::serve(s, &view, &status));
                    if let Err(error) = answered {
                        eprintln!("fs browser: {error}");
                    }
                    false
                }
                Ok(_) => since.elapsed() < IDLE,
                Err(error) => error.kind() == ErrorKind::WouldBlock && since.elapsed() < IDLE,
            }
        });
    }
}
