//! The guest's side of netplay without a window, to test a host from a script.
//!
//! ```text
//! cargo run --release -p shocora_tests --example netclient -- <HOST[:PORT]> <SECONDS> [OUT_DIR]
//! ```
//!
//! Reports the link every second and how much arrived, and saves pictures to `OUT_DIR`.
//! * `SHOCORA_PRESS="second,button,seconds;..."`: scripted buttons of the second player.
//! * `SHOCORA_NET_PASSWORD`: the host's password, if it has one.
use std::{
    path::{Path, PathBuf},
    sync::{Arc, Mutex, PoisonError},
    time::{Duration, Instant},
};

use shocora_netplay::{Client, ClientEvents, Display, Frame, GuestInput, HostStatus};
use wiiu_emu::GamePadState;

#[derive(Debug, Default)]
struct Received {
    frames: u64,
    gamepad_frames: u64,
    frame_bytes: u64,
    samples: u64,
    size: (u32, u32),
    gamepad_size: (u32, u32),
    status: Option<HostStatus>,
}

struct Recorder {
    received: Arc<Mutex<Received>>,
    out: Option<PathBuf>,
}

/// Saves a picture as a PNG file.
fn save_png(path: &Path, frame: &Frame) -> Result<(), Box<dyn std::error::Error>> {
    let file = std::io::BufWriter::new(std::fs::File::create(path)?);
    let mut encoder = png::Encoder::new(file, frame.width, frame.height);

    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    encoder.write_header()?.write_image_data(&frame.rgba)?;

    Ok(())
}

impl ClientEvents for Recorder {
    fn frame(&mut self, frame: Frame) {
        let (name, every, number) = {
            let mut received = self.received.lock().unwrap_or_else(PoisonError::into_inner);
            received.frame_bytes += frame.rgba.len() as u64;

            if frame.display == Display::Tv {
                received.frames += 1;
                received.size = (frame.width, frame.height);
                ("net", 20, received.frames)
            } else {
                received.gamepad_frames += 1;
                received.gamepad_size = (frame.width, frame.height);
                ("net_gamepad", 10, received.gamepad_frames)
            }
        };

        // One picture per second of each screen.
        if let Some(dir) = self.out.as_ref().filter(|_| number % every == 1) {
            if let Err(error) = save_png(&dir.join(format!("{name}_{number:06}.png")), &frame) {
                eprintln!("picture {number}: {error}");
            }
        }
    }

    fn audio(&mut self, samples: &[i16]) {
        self.received.lock().unwrap_or_else(PoisonError::into_inner).samples +=
            samples.len() as u64;
    }

    fn status(&mut self, status: HostStatus) {
        self.received.lock().unwrap_or_else(PoisonError::into_inner).status = Some(status);
    }

    fn changed(&mut self) {}
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let address = args.next().ok_or("usage: netclient <HOST[:PORT]> <SECONDS> [OUT_DIR]")?;
    let seconds: u64 = args.next().ok_or("seconds?")?.parse()?;
    let out = args.next().map(PathBuf::from);

    if let Some(dir) = &out {
        std::fs::create_dir_all(dir)?;
    }

    let presses: Vec<(u64, u32, u64)> = std::env::var("SHOCORA_PRESS")
        .unwrap_or_default()
        .split(';')
        .filter_map(|item| {
            let mut parts = item.split(',');
            Some((
                parts.next()?.trim().parse().ok()?,
                u32::from_str_radix(parts.next()?.trim(), 16).ok()?,
                parts.next()?.trim().parse().ok()?,
            ))
        })
        .collect();

    let received = Arc::new(Mutex::new(Received::default()));
    let password = std::env::var("SHOCORA_NET_PASSWORD").unwrap_or_default();
    let client = Client::start(&address, &password, {
        let received = Arc::clone(&received);
        move || Recorder { received, out }
    })?;

    let start = Instant::now();
    let mut last_report = start;
    let mut last_bits = None;

    while start.elapsed() < Duration::from_secs(seconds) {
        let now = start.elapsed().as_secs();
        let bits = presses
            .iter()
            .filter(|(at, _, length)| now >= *at && now < at + length)
            .fold(0, |bits, (_, button, _)| bits | button);

        if last_bits != Some(bits) {
            client.set_input(GuestInput {
                pad: GamePadState::from_button_bits(bits),
                to_gamepad: false,
            });
            last_bits = Some(bits);
        }

        if last_report.elapsed() >= Duration::from_secs(1) {
            last_report = Instant::now();
            let received = received.lock().unwrap_or_else(PoisonError::into_inner);

            println!(
                "{now:>4}s {:?} · TV {} {:?} · GamePad {} {:?} · {:.1} s of sound · {:?}",
                client.state(),
                received.frames,
                received.size,
                received.gamepad_frames,
                received.gamepad_size,
                received.samples as f64 / 2.0 / 16_000.0,
                received.status,
            );
            drop(received);
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    Ok(())
}
