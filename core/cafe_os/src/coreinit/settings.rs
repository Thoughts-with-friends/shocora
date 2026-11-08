//! System settings: `MCP_GetSysProdSettings` and `UCReadSysConfig`.
use wiiu_hle_macros::hle_export;
use wiiu_memory::GuestPtr;

use crate::hle::{HleContext, HleError};

/// Console settings the OS reports to titles.
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub struct SystemSettings {
    /// `CafeConsoleRegion` bit: 1 JPN, 2 USA, 4 EUR.
    pub region: u8,
    /// `cafe.language`: 0 Japanese, 1 English, ...
    pub language: u32,
}

impl Default for SystemSettings {
    fn default() -> Self {
        Self { region: region::JPN, language: 0 }
    }
}

/// `CafeConsoleRegion` values.
pub mod region {
    pub const JPN: u8 = 0x1;
    pub const USA: u8 = 0x2;
    pub const EUR: u8 = 0x4;
}

/// `sizeof(SysProdSettings)`.
const SYS_PROD_SETTINGS_SIZE: u32 = 0x46;
const SYS_PROD_PLATFORM_REGION: u32 = 0x03;
const SYS_PROD_GAME_REGION: u32 = 0x0B;

/// `sizeof(UCParam)`: name[0x40], 4 words, result pointer.
const UC_PARAM_SIZE: u32 = 0x54;
const UC_PARAM_RESULT: u32 = 0x50;

/// Handle returned by `MCP_Open` / `UCOpen` (any non-negative value works).
const HANDLE: i32 = 1;

#[hle_export(lib = "coreinit", name = "MCP_Open")]
fn mcp_open(_ctx: &mut HleContext<'_>) -> i32 {
    HANDLE
}

#[hle_export(lib = "coreinit", name = "MCP_Close")]
fn mcp_close(_ctx: &mut HleContext<'_>, _handle: i32) -> i32 {
    0
}

#[hle_export(lib = "coreinit", name = "MCP_GetSysProdSettings")]
fn mcp_get_sys_prod_settings(
    ctx: &mut HleContext<'_>,
    _handle: i32,
    out: GuestPtr<u8>,
) -> Result<i32, HleError> {
    let region = ctx.os.settings.region;
    ctx.memory.fill(out.address(), SYS_PROD_SETTINGS_SIZE, 0)?;
    ctx.memory.write_u8(out.address() + SYS_PROD_PLATFORM_REGION, region)?;
    ctx.memory.write_u8(out.address() + SYS_PROD_GAME_REGION, region)?;
    Ok(0)
}

#[hle_export(lib = "coreinit", name = "UCOpen")]
fn uc_open(_ctx: &mut HleContext<'_>) -> i32 {
    HANDLE
}

#[hle_export(lib = "coreinit", name = "UCClose")]
fn uc_close(_ctx: &mut HleContext<'_>, _handle: i32) -> i32 {
    0
}

/// A value written back for one setting: its width and contents.
#[derive(serde::Serialize, serde::Deserialize)]
enum Setting {
    U8(u8),
    U16(u16),
    U32(u32),
}

/// Values for the settings games ask for (same answers as Cemu).
fn lookup(settings: SystemSettings, name: &str) -> Option<Setting> {
    Some(match name.to_ascii_lowercase().as_str() {
        "cafe.language" => Setting::U32(settings.language),
        "cafe.cntry_reg" => Setting::U32(match settings.region {
            region::USA => 49,
            region::EUR => 110,
            _ => 1,
        }),
        "cafe.initial_launch" => Setting::U8(2),
        "cafe.eula_version" | "parent.enable" => Setting::U32(0),
        "cafe.version" => Setting::U16(0),
        "cafe.eula_agree"
        | "cafe.eco"
        | "cafe.fast_boot"
        | "nn.act.account_repaired"
        | "p_acct1.net_communication_on_game"
        | "p_acct1.int_movie"
        | "p_acct1.network_launcher" => Setting::U8(0),
        _ => return None,
    })
}

/// Fills each `UCParam`'s result; unknown settings are logged and left untouched.
#[hle_export(lib = "coreinit", name = "UCReadSysConfig")]
fn uc_read_sys_config(
    ctx: &mut HleContext<'_>,
    _handle: i32,
    count: u32,
    params: GuestPtr<u8>,
) -> Result<i32, HleError> {
    for i in 0..count {
        let param = params.address() + i * UC_PARAM_SIZE;
        let name = ctx.memory.read_c_string(param, 0x40)?;
        let result = ctx.memory.read_u32(param + UC_PARAM_RESULT)?;
        match (lookup(ctx.os.settings, &name), result) {
            (_, 0) => {}
            (Some(Setting::U8(v)), at) => ctx.memory.write_u8(at, v)?,
            (Some(Setting::U16(v)), at) => ctx.memory.write_u16(at, v)?,
            (Some(Setting::U32(v)), at) => ctx.memory.write_u32(at, v)?,
            (None, _) => log::warn!("UCReadSysConfig: unknown setting {name:?}"),
        }
    }
    Ok(0)
}

#[cfg(test)]
mod tests {
    use crate::hle::test_env::{SCRATCH, TestEnv};

    #[test]
    fn reads_language_and_region() {
        let mut env = TestEnv::new();
        env.memory.write_c_string(SCRATCH, "cafe.language").unwrap();
        env.memory.write_u32(SCRATCH + 0x50, SCRATCH + 0x100).unwrap();
        env.memory.write_u32(SCRATCH + 0x100, 0xFFFF_FFFF).unwrap();
        assert_eq!(env.call("coreinit", "UCReadSysConfig", &[1, 1, SCRATCH]), 0);
        assert_eq!(env.memory.read_u32(SCRATCH + 0x100).unwrap(), 0);

        env.call("coreinit", "MCP_GetSysProdSettings", &[1, SCRATCH + 0x200]);
        assert_eq!(env.memory.read_u8(SCRATCH + 0x203).unwrap(), super::region::JPN);
    }
}
