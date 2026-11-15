//! Motion sensors of the host's own controller, read straight from its HID reports: no
//! driver or server program in between.
//!
//! Supported: DualShock 4, DualSense (Edge) over USB or Bluetooth, and the Switch Pro
//! Controller and Joy-Con, whose sensors are switched on first. Readings go through
//! [`SensorFusion`] into GamePad motion; the controller held flat in both hands is the
//! GamePad held flat.
use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

use hidapi::{HidApi, HidDevice};
use wiiu_peripherals::{Motion, SensorFusion};

const SONY: u16 = 0x054C;
const NINTENDO: u16 = 0x057E;

/// Which report layout a controller uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Family {
    DualShock4,
    DualSense,
    Switch,
}

fn family(vendor: u16, product: u16) -> Option<Family> {
    match (vendor, product) {
        (SONY, 0x05C4 | 0x09CC | 0x0BA0) => Some(Family::DualShock4),
        (SONY, 0x0CE6 | 0x0DF2) => Some(Family::DualSense),
        (NINTENDO, 0x2006 | 0x2007 | 0x2009) => Some(Family::Switch),
        _ => None,
    }
}

/// Nominal sensor scales (the full ranges are ±2000 deg/s and ±4 g / ±8 g).
const SONY_GYRO_PER_DEGREE: f32 = 16.384;
const SONY_ACC_PER_G: f32 = 8192.0;
const SWITCH_DEGREES_PER_COUNT: f32 = 4000.0 / 65535.0;
const SWITCH_ACC_PER_G: f32 = 4096.0;
/// A Switch report carries three samples, 5 ms apart.
const SWITCH_SAMPLE_SECONDS: f32 = 0.005;

/// One reading in the controller's own axes: deg/s and g.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Reading {
    gyro: [f32; 3],
    acc: [f32; 3],
}

fn i16_at(report: &[u8], at: usize) -> Option<f32> {
    Some(f32::from(i16::from_le_bytes([*report.get(at)?, *report.get(at + 1)?])))
}

fn triple(report: &[u8], at: usize, scale: f32) -> Option<[f32; 3]> {
    Some([
        i16_at(report, at)? / scale,
        i16_at(report, at + 2)? / scale,
        i16_at(report, at + 4)? / scale,
    ])
}

/// Sony axes are X right, Y out of the face, Z toward the player. GamePad axes are X
/// right, Y away from the player, Z out of the face.
fn sony_to_gamepad([x, y, z]: [f32; 3]) -> [f32; 3] {
    [x, -z, y]
}

/// Switch axes are X away from the player, Y left, Z out of the face.
fn switch_to_gamepad([x, y, z]: [f32; 3]) -> [f32; 3] {
    [-y, x, z]
}

/// The readings in one input report, in GamePad axes, each with the seconds it covers
/// (`None`: time it by the clock).
fn parse(family: Family, report: &[u8]) -> Vec<(Reading, Option<f32>)> {
    let sony = |gyro_at: usize| -> Option<Reading> {
        let gyro = triple(report, gyro_at, SONY_GYRO_PER_DEGREE)?;
        let acc = triple(report, gyro_at + 6, SONY_ACC_PER_G)?;
        Some(Reading { gyro: sony_to_gamepad(gyro), acc: sony_to_gamepad(acc) })
    };

    let reading = match (family, report.first()) {
        // USB report 0x01 and Bluetooth reports 0x11 / 0x31 (two / one bytes more at the front).
        (Family::DualShock4, Some(0x01)) if report.len() >= 64 => sony(13),
        (Family::DualShock4, Some(0x11)) => sony(15),
        (Family::DualSense, Some(0x01)) if report.len() >= 64 => sony(16),
        (Family::DualSense, Some(0x31)) => sony(17),
        (Family::Switch, Some(0x30)) => {
            return (0..3)
                .filter_map(|sample| {
                    let at = 13 + sample * 12;
                    let acc = triple(report, at, SWITCH_ACC_PER_G)?;
                    let gyro = triple(report, at + 6, 1.0 / SWITCH_DEGREES_PER_COUNT)?;
                    let reading =
                        Reading { gyro: switch_to_gamepad(gyro), acc: switch_to_gamepad(acc) };
                    Some((reading, Some(SWITCH_SAMPLE_SECONDS)))
                })
                .collect();
        }
        _ => None,
    };
    reading.map(|reading| (reading, None)).into_iter().collect()
}

/// Asks a controller to send its sensors.
fn start(family: Family, device: &HidDevice) {
    match family {
        // Reading the calibration switches Bluetooth controllers to their full reports.
        Family::DualShock4 | Family::DualSense => {
            for id in [0x02, 0x05] {
                let mut buffer = [0_u8; 64];
                buffer[0] = id;
                let _ = device.get_feature_report(&mut buffer);
            }
        }

        Family::Switch => {
            // Over USB: handshake and keep talking HID (refused over Bluetooth: harmless).
            for command in [[0x80, 0x02], [0x80, 0x04]] {
                let _ = device.write(&command);
                let _ = device.read_timeout(&mut [0_u8; 64], 50);
            }

            // Sensors on, then the full report with them (0x30).
            for (counter, (command, argument)) in (0_u8..).zip([(0x40, 0x01), (0x03, 0x30)]) {
                let mut report = [0_u8; 49];
                report[0] = 0x01;
                report[1] = counter;
                report[2..10].copy_from_slice(&[0x00, 0x01, 0x40, 0x40, 0x00, 0x01, 0x40, 0x40]);
                report[10] = command;
                report[11] = argument;
                let _ = device.write(&report);
                let _ = device.read_timeout(&mut [0_u8; 64], 50);
            }
        }
    }
}

#[derive(Debug, Default)]
struct Shared {
    motion: Option<(Motion, Instant)>,
    device: Option<String>,
}

/// Motion older than this is from a controller that went away.
const STALE: Duration = Duration::from_millis(500);
/// How often to look for a controller while none is open.
const SEARCH_EVERY: Duration = Duration::from_secs(2);

/// A thread reading the motion sensors of the first supported controller.
#[derive(Debug)]
pub struct ControllerMotion {
    shared: Arc<Mutex<Shared>>,
    stop: Arc<AtomicBool>,
}

impl ControllerMotion {
    /// Starts looking for a controller.
    pub fn start() -> Self {
        let shared = Arc::new(Mutex::new(Shared::default()));
        let stop = Arc::new(AtomicBool::new(false));
        let (thread_shared, thread_stop) = (Arc::clone(&shared), Arc::clone(&stop));
        let spawned = thread::Builder::new()
            .name("controller motion".to_owned())
            .spawn(move || run(&thread_shared, &thread_stop));

        if let Err(error) = spawned {
            log::warn!("controller motion is unavailable: {error}");
        }
        Self { shared, stop }
    }

    /// The latest motion, while a controller sends it.
    pub fn motion(&self) -> Option<Motion> {
        let shared = self.shared.lock().ok()?;
        shared.motion.filter(|(_, at)| at.elapsed() < STALE).map(|(motion, _)| motion)
    }

    /// The name of the controller read, if one is open.
    pub fn device(&self) -> Option<String> {
        self.shared.lock().ok()?.device.clone()
    }
}

impl Drop for ControllerMotion {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

fn run(shared: &Mutex<Shared>, stop: &AtomicBool) {
    let mut api = match HidApi::new() {
        Ok(api) => api,
        Err(error) => {
            log::warn!("controller motion is unavailable: {error}");
            return;
        }
    };

    while !stop.load(Ordering::Relaxed) {
        if let Some((family, name, device)) = open_first(&mut api) {
            log::info!("controller motion: {name}");

            if let Ok(mut shared) = shared.lock() {
                shared.device = Some(name);
            }

            start(family, &device);
            read(family, &device, shared, stop);

            if let Ok(mut shared) = shared.lock() {
                *shared = Shared::default();
            }
        }
        thread::sleep(SEARCH_EVERY);
    }
}

fn open_first(api: &mut HidApi) -> Option<(Family, String, HidDevice)> {
    api.refresh_devices().ok()?;
    api.device_list().find_map(|info| {
        let family = family(info.vendor_id(), info.product_id())?;
        let device = info.open_device(api).ok()?;
        let name = info.product_string().unwrap_or("controller").to_owned();
        Some((family, name, device))
    })
}

/// Reads until the controller goes away or reading stops.
fn read(family: Family, device: &HidDevice, shared: &Mutex<Shared>, stop: &AtomicBool) {
    let mut fusion = SensorFusion::default();
    let mut last = Instant::now();
    let mut buffer = [0_u8; 128];

    while !stop.load(Ordering::Relaxed) {
        let length = match device.read_timeout(&mut buffer, 100) {
            Ok(0) => continue,
            Ok(length) => length,
            Err(error) => {
                log::info!("controller motion stopped: {error}");
                return;
            }
        };

        let readings = parse(family, &buffer[..length]);

        if readings.is_empty() {
            continue;
        }

        let elapsed = last.elapsed().as_secs_f32();
        last = Instant::now();

        let mut motion = None;
        for (reading, seconds) in readings {
            motion = Some(fusion.update(reading.gyro, reading.acc, seconds.unwrap_or(elapsed)));
        }

        if let (Some(motion), Ok(mut shared)) = (motion, shared.lock()) {
            shared.motion = Some((motion, Instant::now()));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn put(report: &mut [u8], at: usize, values: [i16; 3]) {
        for (i, value) in values.into_iter().enumerate() {
            report[at + i * 2..at + i * 2 + 2].copy_from_slice(&value.to_le_bytes());
        }
    }

    /// Intent: a DualShock 4 lying flat on USB reads as a GamePad lying flat, and its turn
    /// around the vertical reaches GamePad Z.
    /// Pass: 1 g out of the face is +Z force; Sony yaw (its Y) is GamePad Z, in deg/s.
    #[test]
    fn dualshock4_usb_report() {
        let mut report = [0_u8; 64];
        report[0] = 0x01;

        put(&mut report, 13, [0, 1638, 0]);
        put(&mut report, 19, [0, 8192, 0]);

        let readings = parse(Family::DualShock4, &report);
        let (reading, seconds) = readings[0];

        assert_eq!(seconds, None);
        assert!((reading.gyro[2] - 100.0).abs() < 0.1, "{:?}", reading.gyro);
        assert_eq!(reading.acc, [0.0, 0.0, 1.0]);
    }

    /// Intent: DualSense Bluetooth reports are one byte later than USB ones.
    /// Pass: values placed at the Bluetooth offsets come back.
    #[test]
    fn dualsense_bluetooth_report() {
        let mut report = [0_u8; 78];
        report[0] = 0x31;
        put(&mut report, 23, [0, 0, -8192]);

        let (reading, _) = parse(Family::DualSense, &report)[0];
        assert_eq!(reading.acc, [0.0, 1.0, 0.0]);
    }

    /// Intent: a Switch report gives its three samples, 5 ms each, in GamePad axes.
    /// Pass: three readings; 1 g out of the face is +Z; Switch Y (left) is GamePad -X.
    #[test]
    fn switch_full_report() {
        let mut report = [0_u8; 49];
        report[0] = 0x30;

        for sample in 0..3 {
            put(&mut report, 13 + sample * 12, [0, 0, 4096]);
            put(&mut report, 19 + sample * 12, [0, 1638, 0]);
        }

        let readings = parse(Family::Switch, &report);
        assert_eq!(readings.len(), 3);

        let (reading, seconds) = readings[2];
        assert_eq!(seconds, Some(SWITCH_SAMPLE_SECONDS));
        assert_eq!(reading.acc, [0.0, 0.0, 1.0]);
        assert!((reading.gyro[0] + 100.0).abs() < 0.1, "{:?}", reading.gyro);
    }

    /// Intent: other reports (buttons-only modes, other devices) are ignored.
    /// Pass: nothing is read from them.
    #[test]
    fn other_reports_are_ignored() {
        assert!(parse(Family::DualShock4, &[0x01, 0, 0]).is_empty());
        assert!(parse(Family::Switch, &[0x3F; 12]).is_empty());
        assert_eq!(family(0x045E, 0x028E), None);
    }
}
