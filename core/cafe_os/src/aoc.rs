//! `nn_aoc`: downloadable content ("add-on content").
//!
//! The front end finds the title's content and mounts it at [`AocTitle::path`]; this
//! library lists it to the title, which then opens files under that path.
use wiiu_hle_macros::hle_export;

use crate::hle::{HleContext, HleError, HleExport};

/// `AOC_ERROR_OK`.
const AOC_OK: i32 = 0;
/// `sizeof(AOC_TitleListType)`.
const TITLE_SIZE: u32 = 0x68;
/// Offsets inside `AOC_TitleListType`.
mod title {
    pub const TITLE_ID: u32 = 0x00;
    pub const GROUP_ID: u32 = 0x08;
    pub const VERSION: u32 = 0x0C;
    pub const PATH: u32 = 0x0E;
    /// Room for the path, its terminator included.
    pub const PATH_SIZE: usize = 88;
}

/// Downloadable content installed for the running title.
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub struct AocTitle {
    pub title_id: u64,
    pub version: u16,
}

impl AocTitle {
    /// Where its `content/` is mounted.
    #[inline]
    pub fn path(self) -> String {
        format!("/vol/aoc{:016x}", self.title_id)
    }

    /// The group every title of one game shares, as in its `meta.xml`.
    #[allow(clippy::cast_possible_truncation, reason = "masked to 16 bits")]
    const fn group_id(self) -> u32 {
        ((self.title_id >> 8) & 0xFFFF) as u32
    }
}

#[hle_export(lib = "nn_aoc", name = "AOC_Initialize")]
fn aoc_initialize(_ctx: &mut HleContext<'_>) -> i32 {
    AOC_OK
}

#[hle_export(lib = "nn_aoc", name = "AOC_Finalize")]
fn aoc_finalize(_ctx: &mut HleContext<'_>) -> i32 {
    AOC_OK
}

/// Nothing is kept in the work buffer, but titles allocate what this says.
#[hle_export(lib = "nn_aoc", name = "AOC_CalculateWorkBufferSize")]
fn aoc_calculate_work_buffer_size(_ctx: &mut HleContext<'_>, count: u32) -> u32 {
    0x80 + count.min(256) * 0x61
}

#[hle_export(lib = "nn_aoc", name = "AOC_ListTitle")]
fn aoc_list_title(
    ctx: &mut HleContext<'_>,
    count_out: u32,
    list: u32,
    max_count: u32,
    _work: u32,
    _work_size: u32,
) -> Result<i32, HleError> {
    let titles: Vec<AocTitle> = ctx.os.aoc.iter().copied().take(max_count as usize).collect();
    for (index, aoc) in (0_u32..).zip(&titles) {
        let at = list + index * TITLE_SIZE;
        ctx.memory.write_bytes(at, &[0; TITLE_SIZE as usize])?;
        ctx.memory.write_bytes(at + title::TITLE_ID, &aoc.title_id.to_be_bytes())?;
        ctx.memory.write_u32(at + title::GROUP_ID, aoc.group_id())?;
        ctx.memory.write_u16(at + title::VERSION, aoc.version)?;
        write_path(ctx, at + title::PATH, *aoc)?;
    }
    if count_out != 0 {
        ctx.memory.write_u32(count_out, u32::try_from(titles.len()).unwrap_or(u32::MAX))?;
    }
    Ok(AOC_OK)
}

/// Writes where the content listed at `title` is mounted to `path_out`.
#[hle_export(lib = "nn_aoc", name = "AOC_OpenTitle")]
fn aoc_open_title(
    ctx: &mut HleContext<'_>,
    path_out: u32,
    title: u32,
    _work: u32,
    _work_size: u32,
) -> Result<i32, HleError> {
    let mut id = [0; 8];
    ctx.memory.read_bytes(title + title::TITLE_ID, &mut id)?;
    let title_id = u64::from_be_bytes(id);
    let aoc = ctx.os.aoc.iter().copied().find(|aoc| aoc.title_id == title_id);
    let aoc = aoc.unwrap_or(AocTitle { title_id, version: 0 });
    write_path(ctx, path_out, aoc)?;
    Ok(AOC_OK)
}

#[hle_export(lib = "nn_aoc", name = "AOC_CloseTitle")]
fn aoc_close_title(_ctx: &mut HleContext<'_>, _title: u32) -> i32 {
    AOC_OK
}

/// Every piece of installed content counts as bought.
#[hle_export(lib = "nn_aoc", name = "AOC_GetPurchaseInfo")]
fn aoc_get_purchase_info(
    ctx: &mut HleContext<'_>,
    flags_out: u32,
    _title_id: u64,
    _contents: u32,
    count: u32,
    _work: u32,
    _work_size: u32,
) -> Result<i32, HleError> {
    for index in 0..count {
        ctx.memory.write_u32(flags_out + index * 4, 1)?;
    }
    Ok(AOC_OK)
}

fn write_path(ctx: &mut HleContext<'_>, at: u32, aoc: AocTitle) -> Result<(), HleError> {
    let path = aoc.path();
    debug_assert!(path.len() < title::PATH_SIZE);
    ctx.memory.write_c_string(at, &path)?;
    Ok(())
}

pub const EXPORTS: &[HleExport] = &[
    AOC_INITIALIZE,
    AOC_FINALIZE,
    AOC_CALCULATE_WORK_BUFFER_SIZE,
    AOC_LIST_TITLE,
    AOC_OPEN_TITLE,
    AOC_CLOSE_TITLE,
    AOC_GET_PURCHASE_INFO,
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_and_groups_follow_the_title_id() {
        let aoc = AocTitle { title_id: 0x0005_000C_101C_9300, version: 0x50 };
        assert_eq!(aoc.path(), "/vol/aoc0005000c101c9300");
        assert_eq!(aoc.group_id(), 0x1C93);
    }
}
