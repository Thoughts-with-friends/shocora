//! Network-service and account libraries.
//!
//! shocora is offline: accounts report a
//! single local user and everything else stays at its "not connected" default
//! (unimplemented imports return 0, which `nn::Result` reads as success).
use wiiu_hle_macros::hle_export;
use wiiu_peripherals::VpadButtons;

use crate::{
    coreinit::fs::mutate::PERSISTENT_ID,
    hle::{HleContext, HleError, HleExport},
};

/// `nn::act` slot of the only account.
const ACCOUNT_SLOT: u8 = 1;
/// Identifies the local user to titles that key data by account.
const ACCOUNT_UUID: [u8; 16] = *b"shocora localusr";
const ACCOUNT_ID: &str = "shocora";

#[hle_export(lib = "nn_act", name = "GetSlotNo__Q2_2nn3actFv")]
fn act_get_slot_no(_ctx: &mut HleContext<'_>) -> u8 {
    ACCOUNT_SLOT
}

#[hle_export(lib = "nn_act", name = "GetDefaultAccount__Q2_2nn3actFv")]
fn act_get_default_account(_ctx: &mut HleContext<'_>) -> u8 {
    ACCOUNT_SLOT
}

#[hle_export(lib = "nn_act", name = "GetNumOfAccounts__Q2_2nn3actFv")]
fn act_get_num_of_accounts(_ctx: &mut HleContext<'_>) -> u32 {
    1
}

#[hle_export(lib = "nn_act", name = "IsSlotOccupied__Q2_2nn3actFUc")]
fn act_is_slot_occupied(_ctx: &mut HleContext<'_>, slot: u8) -> bool {
    slot == ACCOUNT_SLOT
}

#[hle_export(lib = "nn_act", name = "GetPersistentId__Q2_2nn3actFv")]
fn act_get_persistent_id(_ctx: &mut HleContext<'_>) -> u32 {
    PERSISTENT_ID
}

#[hle_export(lib = "nn_act", name = "GetPersistentIdEx__Q2_2nn3actFUc")]
fn act_get_persistent_id_ex(_ctx: &mut HleContext<'_>, slot: u8) -> u32 {
    if slot == ACCOUNT_SLOT { PERSISTENT_ID } else { 0 }
}

#[hle_export(lib = "nn_act", name = "GetUuid__Q2_2nn3actFP7ACTUuid")]
fn act_get_uuid(ctx: &mut HleContext<'_>, out: u32) -> Result<u32, HleError> {
    ctx.memory.write_bytes(out, &ACCOUNT_UUID)?;
    Ok(0)
}

#[hle_export(lib = "nn_act", name = "GetAccountId__Q2_2nn3actFPc")]
fn act_get_account_id(ctx: &mut HleContext<'_>, out: u32) -> Result<u32, HleError> {
    ctx.memory.write_c_string(out, ACCOUNT_ID)?;
    Ok(0)
}

/// Writes the Mii name as big-endian UTF-16.
#[hle_export(lib = "nn_act", name = "GetMiiName__Q2_2nn3actFPw")]
fn act_get_mii_name(ctx: &mut HleContext<'_>, out: u32) -> Result<u32, HleError> {
    let mut at = out;
    for unit in ACCOUNT_ID.encode_utf16().chain([0]) {
        ctx.memory.write_u16(at, unit)?;
        at += 2;
    }
    Ok(0)
}

/// `WPAD_ERROR_NO_CONTROLLER`: nothing on this channel.
const WPAD_NO_CONTROLLER: i32 = -1;
/// `KPAD_READ_ERR_NO_SAMPLE_DATA`.
const KPAD_NO_SAMPLE_DATA: i32 = -1;
/// `KPAD_READ_ERR_NO_CONTROLLER`.
const KPAD_NO_CONTROLLER: i32 = -2;
/// `WPAD_EXT_DEV_NOT_FOUND`: what `WPADProbe` reports for an empty channel.
const WPAD_DEV_NOT_FOUND: u32 = 0xFD;
/// `WPAD_DEV_URCC`: the Wii U Pro Controller.
const WPAD_DEV_PRO: u8 = 31;
/// `WPAD_FMT_URCC`.
const WPAD_FORMAT_PRO: u8 = 22;
/// Channels `WPAD` serves.
pub const PRO_CHANNELS: usize = 4;
/// A controller delivers a new sample at most this often; titles read until there is none.
const KPAD_SAMPLE_INTERVAL: std::time::Duration = std::time::Duration::from_millis(1);
/// `sizeof(KPADStatus)`.
const KPAD_STATUS_SIZE: u32 = 0xF0;

/// Offsets inside `KPADStatus`.
mod kpad {
    pub const DEVICE: u32 = 0x5C;
    pub const FORMAT: u32 = 0x5F;
    pub const PRO_HOLD: u32 = 0x60;
    pub const PRO_TRIGGER: u32 = 0x64;
    pub const PRO_RELEASE: u32 = 0x68;
    pub const PRO_LEFT_STICK: u32 = 0x6C;
    pub const PRO_RIGHT_STICK: u32 = 0x74;
    /// Wired: the battery never runs out.
    pub const PRO_CABLE: u32 = 0x80;
}

/// GamePad buttons to the bits of the Pro Controller.
fn pro_buttons(held: VpadButtons) -> u32 {
    const MAP: [(VpadButtons, u32); 17] = [
        (VpadButtons::UP, 0x1),
        (VpadButtons::LEFT, 0x2),
        (VpadButtons::ZR, 0x4),
        (VpadButtons::X, 0x8),
        (VpadButtons::A, 0x10),
        (VpadButtons::Y, 0x20),
        (VpadButtons::B, 0x40),
        (VpadButtons::ZL, 0x80),
        (VpadButtons::R, 0x200),
        (VpadButtons::PLUS, 0x400),
        (VpadButtons::HOME, 0x800),
        (VpadButtons::MINUS, 0x1000),
        (VpadButtons::L, 0x2000),
        (VpadButtons::DOWN, 0x4000),
        (VpadButtons::RIGHT, 0x8000),
        (VpadButtons::STICK_R, 0x1_0000),
        (VpadButtons::STICK_L, 0x2_0000),
    ];
    MAP.iter().filter(|(button, _)| held.contains(*button)).fold(0, |bits, (_, bit)| bits | bit)
}
/// Work area `KPADSetMplsWorkarea` expects.
const KPAD_MPLS_WORK_SIZE: u32 = 0x5FE0;

#[hle_export(lib = "padscore", name = "WPADProbe")]
fn wpad_probe(ctx: &mut HleContext<'_>, channel: u32, device: u32) -> Result<i32, HleError> {
    let connected = ctx.os.pro_controllers.get(channel as usize).is_some_and(Option::is_some);
    if device != 0 {
        let kind = if connected { u32::from(WPAD_DEV_PRO) } else { WPAD_DEV_NOT_FOUND };
        ctx.memory.write_u32(device, kind)?;
    }
    Ok(if connected { 0 } else { WPAD_NO_CONTROLLER })
}

/// Fills `buffer` with one sample of `channel`; returns the samples written and the error.
fn kpad_read(
    ctx: &mut HleContext<'_>,
    channel: u32,
    buffer: u32,
    count: u32,
) -> Result<(u32, i32), HleError> {
    let index = channel as usize;
    let Some(pad) = ctx.os.pro_controllers.get(index).copied().flatten() else {
        return Ok((0, KPAD_NO_CONTROLLER));
    };
    let now = std::time::Instant::now();
    let too_soon =
        ctx.os.pro_last_read[index].is_some_and(|last| now - last < KPAD_SAMPLE_INTERVAL);
    if count == 0 || buffer == 0 || too_soon {
        return Ok((0, KPAD_NO_SAMPLE_DATA));
    }
    ctx.os.pro_last_read[index] = Some(now);
    let (trigger, release) = ctx.os.pro_edges[index].update(pad.held);
    ctx.memory.fill(buffer, KPAD_STATUS_SIZE, 0)?;
    ctx.memory.write_u8(buffer + kpad::DEVICE, WPAD_DEV_PRO)?;
    ctx.memory.write_u8(buffer + kpad::FORMAT, WPAD_FORMAT_PRO)?;
    ctx.memory.write_u32(buffer + kpad::PRO_HOLD, pro_buttons(pad.held))?;
    ctx.memory.write_u32(buffer + kpad::PRO_TRIGGER, pro_buttons(trigger))?;
    ctx.memory.write_u32(buffer + kpad::PRO_RELEASE, pro_buttons(release))?;
    for (at, stick) in
        [(kpad::PRO_LEFT_STICK, pad.left_stick), (kpad::PRO_RIGHT_STICK, pad.right_stick)]
    {
        ctx.memory.write_f32(buffer + at, stick.x)?;
        ctx.memory.write_f32(buffer + at + 4, stick.y)?;
    }
    ctx.memory.write_u32(buffer + kpad::PRO_CABLE, 1)?;
    Ok((1, 0))
}

#[hle_export(lib = "padscore", name = "KPADReadEx")]
fn kpad_read_ex(
    ctx: &mut HleContext<'_>,
    channel: u32,
    buffer: u32,
    count: u32,
    error: u32,
) -> Result<u32, HleError> {
    let (samples, result) = kpad_read(ctx, channel, buffer, count)?;
    if error != 0 {
        ctx.memory.write_u32(error, result.cast_unsigned())?;
    }
    Ok(samples)
}

#[hle_export(lib = "padscore", name = "KPADRead")]
fn kpad_read_plain(
    ctx: &mut HleContext<'_>,
    channel: u32,
    buffer: u32,
    count: u32,
) -> Result<u32, HleError> {
    Ok(kpad_read(ctx, channel, buffer, count)?.0)
}

#[hle_export(lib = "padscore", name = "KPADGetMplsWorkSize")]
fn kpad_get_mpls_work_size(_ctx: &mut HleContext<'_>) -> u32 {
    KPAD_MPLS_WORK_SIZE
}

/// `SWKBD`/`SYSApp`-style launches cannot happen; report the title as launched normally.
pub const EXPORTS: &[HleExport] = &[
    ACT_GET_SLOT_NO,
    ACT_GET_DEFAULT_ACCOUNT,
    ACT_GET_NUM_OF_ACCOUNTS,
    ACT_IS_SLOT_OCCUPIED,
    ACT_GET_PERSISTENT_ID,
    ACT_GET_PERSISTENT_ID_EX,
    ACT_GET_UUID,
    ACT_GET_ACCOUNT_ID,
    ACT_GET_MII_NAME,
    WPAD_PROBE,
    KPAD_READ_EX,
    KPAD_READ_PLAIN,
    KPAD_GET_MPLS_WORK_SIZE,
];

#[cfg(test)]
mod tests {
    use wiiu_peripherals::{GamePadState, VpadButtons};

    use super::kpad;
    use crate::hle::test_env::{SCRATCH, TestEnv};

    /// Intent: `WPADProbe` tells titles which channels have a Pro Controller.
    /// Pass: an empty channel returns -1 with device 0xFD; a connected one returns 0 with
    /// device 31 (`WPAD_DEV_URCC`).
    #[test]
    fn probe_reports_connected_pro_controllers() {
        let mut env = TestEnv::new();
        assert_eq!(env.call("padscore", "WPADProbe", &[0, SCRATCH]) as i32, -1);
        assert_eq!(env.memory.read_u32(SCRATCH).unwrap(), 0xFD);
        env.os.pro_controllers[0] = Some(GamePadState::default());
        assert_eq!(env.call("padscore", "WPADProbe", &[0, SCRATCH]), 0);
        assert_eq!(env.memory.read_u32(SCRATCH).unwrap(), 31);
    }

    /// Intent: `KPADReadEx` gives a Pro Controller sample with the buttons in its own bit
    /// layout (A is 0x10, not the GamePad's 0x8000) and edge bits for new presses.
    /// Pass: one sample, no error, hold and trigger both 0x10, format 22 (`WPAD_FMT_URCC`).
    #[test]
    fn kpad_reads_pro_controller_buttons() {
        let mut env = TestEnv::new();
        let pad = GamePadState { held: VpadButtons::A, ..GamePadState::default() };
        env.os.pro_controllers[1] = Some(pad);
        let error = SCRATCH + 0x200;
        assert_eq!(env.call("padscore", "KPADReadEx", &[1, SCRATCH, 1, error]), 1);
        assert_eq!(env.memory.read_u32(error).unwrap(), 0);
        assert_eq!(env.memory.read_u32(SCRATCH + kpad::PRO_HOLD).unwrap(), 0x10);
        assert_eq!(env.memory.read_u32(SCRATCH + kpad::PRO_TRIGGER).unwrap(), 0x10);
        assert_eq!(env.memory.read_u8(SCRATCH + kpad::FORMAT).unwrap(), 22);
    }
}
