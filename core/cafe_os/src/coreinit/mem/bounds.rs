//! Memory areas owned by the process: `OSGetMemBound`, foreground bucket.
use wiiu_hle_macros::hle_export;
use wiiu_mem_addr::{FOREGROUND, FOREGROUND_FREE_SIZE, MEM1};
use wiiu_memory::GuestPtr;

use crate::hle::{HleContext, HleError};

/// `OSMemoryType`.
const MEMORY_TYPE_MEM1: u32 = 1;
const MEMORY_TYPE_MEM2: u32 = 2;

fn write_area(
    ctx: &mut HleContext<'_>,
    address_out: GuestPtr<u32>,
    size_out: GuestPtr<u32>,
    (address, size): (u32, u32),
) -> Result<(), HleError> {
    if !address_out.is_null() {
        address_out.write(ctx.memory, address)?;
    }
    if !size_out.is_null() {
        size_out.write(ctx.memory, size)?;
    }
    Ok(())
}

/// Returns 0 on success, -1 for unknown memory types.
#[hle_export(lib = "coreinit", name = "OSGetMemBound")]
fn os_get_mem_bound(
    ctx: &mut HleContext<'_>,
    memory_type: u32,
    address_out: GuestPtr<u32>,
    size_out: GuestPtr<u32>,
) -> Result<i32, HleError> {
    let area = match memory_type {
        MEMORY_TYPE_MEM1 => (MEM1.start, MEM1.size),
        MEMORY_TYPE_MEM2 => ctx.os.coreinit.heaps.mem2_bounds(),
        _ => return Ok(-1),
    };
    write_area(ctx, address_out, size_out, area)?;
    Ok(0)
}

/// The whole bucket; returns `true` because the title is always in the foreground.
#[hle_export(lib = "coreinit", name = "OSGetForegroundBucket")]
fn os_get_foreground_bucket(
    ctx: &mut HleContext<'_>,
    address_out: GuestPtr<u32>,
    size_out: GuestPtr<u32>,
) -> Result<bool, HleError> {
    write_area(ctx, address_out, size_out, (FOREGROUND.start, FOREGROUND.size))?;
    Ok(true)
}

#[hle_export(lib = "coreinit", name = "OSGetForegroundBucketFreeArea")]
fn os_get_foreground_bucket_free_area(
    ctx: &mut HleContext<'_>,
    address_out: GuestPtr<u32>,
    size_out: GuestPtr<u32>,
) -> Result<bool, HleError> {
    write_area(ctx, address_out, size_out, (FOREGROUND.start, FOREGROUND_FREE_SIZE))?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use wiiu_mem_addr::{FOREGROUND, MEM1};

    use crate::hle::test_env::{SCRATCH, TestEnv};

    /// Intent: titles size their heaps from `OSGetMemBound` and the foreground bucket.
    /// Pass: MEM1 is the whole 32 MiB at its fixed address; an unknown type returns -1;
    /// the foreground bucket starts at its fixed address.
    #[test]
    fn reports_memory_areas() {
        let mut env = TestEnv::new();
        assert_eq!(env.call("coreinit", "OSGetMemBound", &[1, SCRATCH, SCRATCH + 4]), 0);
        assert_eq!(env.memory.read_u32(SCRATCH).unwrap(), MEM1.start);
        assert_eq!(env.memory.read_u32(SCRATCH + 4).unwrap(), MEM1.size);
        assert_eq!(env.call("coreinit", "OSGetMemBound", &[9, SCRATCH, 0]) as i32, -1);
        assert_eq!(env.call("coreinit", "OSGetForegroundBucket", &[SCRATCH, SCRATCH + 4]), 1);
        assert_eq!(env.memory.read_u32(SCRATCH).unwrap(), FOREGROUND.start);
    }
}
