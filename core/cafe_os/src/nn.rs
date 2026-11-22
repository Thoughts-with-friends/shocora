//! Network-service and account libraries.
//!
//! shocora is offline: accounts report a
//! single local user and everything else stays at its "not connected" default
//! (unimplemented imports return 0, which `nn::Result` reads as success).
//!
//! ## Where this file sits
//!
//! ```text
//! Game files ─► Loader ─► CPU ◄─► Memory ◄─► OS (HLE) ─► GPU ─► Screen
//!                                            ▲▲▲▲▲▲▲▲  here
//! Controllers, clock ─► OS (HLE) ─► Sound ─► Speakers
//!
//! ┌────────────────────┐   ┌──────────────────────────┐   ┌────────────────────┐
//! │ online/account     │   │ nn.rs                    │   │ "offline" answers  │
//! │ calls              │──►│ ──────────────────────── │──►│                    │
//! │                    │   │ network & accounts       │   │                    │
//! └────────────────────┘   └──────────────────────────┘   └────────────────────┘
//! ```
//!
//! shocora plays offline: account and online-service libraries answer as a single local user
//! with no network. Terms such as HLE, JIT or shader are explained in
//! `docs/guide/wiiu_term.md`.
use wiiu_hle_macros::hle_export;

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

pub use crate::padscore::PRO_CHANNELS;

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
];
