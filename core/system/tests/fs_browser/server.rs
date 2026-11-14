//! A read-only HTTP view of the emulator's file system: the routes of `index.html`.
//!
//! Only `std`: one request per connection, `GET` only, bound to the loopback address by
//! the caller. Paths are console paths (`/vol/content/…`, `/dev/fsa`, `/proc/…`) resolved
//! through a [`View`] of the emulator, so nothing outside its volumes can be reached.
use std::{
    collections::HashMap,
    io::{BufRead, BufReader, Write},
    net::TcpStream,
    time::Duration,
};

use super::console::View;

/// The page: explorer, tabs and viewer.
pub const PAGE: &str = include_str!("index.html");
/// The page's look.
const STYLE: &str = include_str!("index.css");
/// The page's behaviour.
const SCRIPT: &str = include_str!("index.js");

/// The largest slice of a file one request returns.
pub const MAX_CHUNK: u64 = 1 << 20;

/// What a route answers.
pub struct Response {
    pub status: u16,
    pub kind: &'static str,
    /// The whole file size for `/api/file`, sent as `X-Size`.
    pub size: Option<u64>,
    pub body: Vec<u8>,
}

impl Response {
    fn ok(kind: &'static str, body: impl Into<Vec<u8>>) -> Self {
        Self { status: 200, kind, size: None, body: body.into() }
    }

    fn error(status: u16, message: impl std::fmt::Display) -> Self {
        Self {
            status,
            kind: "text/plain; charset=utf-8",
            size: None,
            body: message.to_string().into(),
        }
    }

    /// The body as text (for tests).
    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }
}

/// `%XX` and `+` decoding of one query value.
fn decode(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => out.push(b' '),
            b'%' => {
                match value.get(i + 1..i + 3).and_then(|hex| u8::from_str_radix(hex, 16).ok()) {
                    Some(byte) => {
                        out.push(byte);
                        i += 2;
                    }
                    None => out.push(b'%'),
                }
            }
            byte => out.push(byte),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn query(raw: &str) -> HashMap<String, String> {
    raw.split('&')
        .filter_map(|pair| pair.split_once('='))
        .map(|(key, value)| (decode(key), decode(value)))
        .collect()
}

/// A JSON string literal.
pub fn json_string(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c if u32::from(c) < 0x20 => out.push_str(&format!("\\u{:04x}", u32::from(c))),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// A guest path the browser may ask for: absolute, and no host separators or drive
/// letters that a host folder would read as its own.
fn guest_path(params: &HashMap<String, String>) -> Result<&str, Response> {
    let path = params.get("path").map_or("/", String::as_str);
    if !path.starts_with('/') || path.contains(['\\', ':']) {
        return Err(Response::error(400, format!("not a guest path: {path}")));
    }
    Ok(path)
}

/// The entries of a folder: what its device lists, plus the generated ones below it
/// (`/`, `/vol`, `/dev` and `/proc` exist only in the view).
fn list(view: &View, path: &str) -> Response {
    let target = view.resolve(path);
    let mut entries: Vec<(String, bool, u64)> = target
        .as_deref()
        .and_then(|target| view.vfs.read_dir(target).ok())
        .map(|list| {
            list.into_iter().map(|e| (e.name, e.metadata.is_dir, e.metadata.size)).collect()
        })
        .unwrap_or_default();
    for entry in view.children(path) {
        if !entries.iter().any(|(name, ..)| *name == entry.0) {
            entries.push(entry);
        }
    }
    let real = target.is_some_and(|target| view.vfs.metadata(&target).is_ok_and(|m| m.is_dir));
    if entries.is_empty() && !real && !view.is_virtual_dir(path) {
        return Response::error(404, format!("no folder {path}"));
    }
    // Folders first, then by name, as an editor's explorer shows them.
    entries.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.to_lowercase().cmp(&b.0.to_lowercase())));
    let items: Vec<String> = entries
        .iter()
        .map(|(name, dir, size)| {
            format!(r#"{{"name":{},"dir":{dir},"size":{size}}}"#, json_string(name))
        })
        .collect();
    Response::ok("application/json", format!("[{}]", items.join(",")))
}

/// A slice of a file: `offset` and `len` (at most [`MAX_CHUNK`]) bytes.
fn file(view: &View, path: &str, params: &HashMap<String, String>) -> Response {
    let number =
        |key: &str, default: u64| params.get(key).and_then(|v| v.parse().ok()).unwrap_or(default);
    if let Some(text) = view.text(path) {
        let size = text.len() as u64;
        let offset = usize::try_from(number("offset", 0).min(size)).unwrap_or(0);
        let body = text.as_bytes()[offset..].to_vec();
        return Response { size: Some(size), ..Response::ok("application/octet-stream", body) };
    }
    let Some(target) = view.resolve(path) else {
        return Response::error(404, format!("no file {path}"));
    };
    let metadata = match view.vfs.metadata(&target) {
        Ok(metadata) if !metadata.is_dir => metadata,
        Ok(_) => return Response::error(400, format!("{path} is a folder")),
        Err(error) => return Response::error(404, error),
    };
    let offset = number("offset", 0).min(metadata.size);
    let len = number("len", MAX_CHUNK).min(MAX_CHUNK).min(metadata.size - offset);
    let mut body = vec![0; usize::try_from(len).unwrap_or(0)];
    match view.vfs.read_at(&target, offset, &mut body) {
        Ok(read) => {
            body.truncate(read);
            Response { size: Some(metadata.size), ..Response::ok("application/octet-stream", body) }
        }
        Err(error) => Response::error(500, error),
    }
}

/// Answers `GET target`. `status` is the JSON of `/api/status`.
pub fn respond(view: &View, status: &str, target: &str) -> Response {
    let (route, raw_query) = target.split_once('?').unwrap_or((target, ""));
    let params = query(raw_query);
    match route {
        "/" | "/index.html" => Response::ok("text/html; charset=utf-8", PAGE),
        "/index.css" => Response::ok("text/css; charset=utf-8", STYLE),
        "/index.js" => Response::ok("text/javascript; charset=utf-8", SCRIPT),
        "/api/status" => Response::ok("application/json", status),
        "/api/dir" => guest_path(&params).map_or_else(|e| e, |path| list(view, path)),
        "/api/file" => guest_path(&params).map_or_else(|e| e, |path| file(view, path, &params)),
        _ => Response::error(404, format!("no route {route}")),
    }
}

/// Reads one request from `stream` and answers it.
///
/// # Errors
/// If the connection fails.
pub fn serve(mut stream: TcpStream, view: &View, status: &str) -> std::io::Result<()> {
    stream.set_nonblocking(false)?;
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut line = String::new();
    reader.read_line(&mut line)?;
    // The headers say nothing a GET route needs.
    let mut header = String::new();
    while reader.read_line(&mut header)? > 2 {
        header.clear();
    }
    let mut parts = line.split_whitespace();
    let response = match (parts.next(), parts.next()) {
        (Some("GET"), Some(target)) => respond(view, status, target),
        _ => Response::error(405, "only GET"),
    };
    let size = response.size.map(|s| format!("X-Size: {s}\r\n")).unwrap_or_default();
    write!(
        stream,
        "HTTP/1.1 {} -\r\nContent-Type: {}\r\nContent-Length: {}\r\n{size}Cache-Control: no-store\r\nConnection: close\r\n\r\n",
        response.status,
        response.kind,
        response.body.len()
    )?;
    stream.write_all(&response.body)?;
    stream.flush()
}
