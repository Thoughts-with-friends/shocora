//! Where translated code lives: large chunks handed out a block at a time.
//!
//! Cranelift's default provider maps memory of its own for nearly every function (rounded
//! up to whole pages, and to 64 KiB of address space on Windows), and starts afresh after
//! every batch is made executable. With hundreds of thousands of blocks that came to
//! gigabytes. Here functions are packed one after the other, and a batch wastes at most
//! the rest of its last page, as Cemu's recompiler packs code into one large buffer.
use std::{io, mem::ManuallyDrop};

use cranelift_jit::{BranchProtection, JITMemoryKind, JITMemoryProvider};
use cranelift_module::{ModuleError, ModuleResult};

/// Address space of one chunk: pages count only once code is written to them.
const CHUNK_SIZE: usize = 64 << 20;

/// One chunk: `[ptr, ptr + finalized)` is executable, `[finalized, cursor)` being written.
struct Chunk {
    /// Never freed on drop: code may still run from it. [`CodeMemory::free_memory`] frees it.
    allocation: ManuallyDrop<region::Allocation>,
    cursor: usize,
    finalized: usize,
}

impl Chunk {
    fn new(size: usize) -> io::Result<Self> {
        let allocation =
            region::alloc(size, region::Protection::READ_WRITE).map_err(io::Error::other)?;
        Ok(Self { allocation: ManuallyDrop::new(allocation), cursor: 0, finalized: 0 })
    }

    fn base(&mut self) -> *mut u8 {
        self.allocation.as_mut_ptr::<u8>()
    }

    /// Room for `size` bytes at `align`, if the chunk has it.
    fn allocate(&mut self, size: usize, align: usize) -> Option<*mut u8> {
        let start = self.cursor.next_multiple_of(align);
        if start + size > self.allocation.len() {
            return None;
        }
        self.cursor = start + size;
        // SAFETY: `start + size` is within the allocation.
        Some(unsafe { self.base().add(start) })
    }

    /// Makes what was written since the last call executable. The rest of the last page
    /// can no longer be written, so the next code starts on a fresh page.
    fn finalize(&mut self) -> ModuleResult<()> {
        let page = region::page::size();
        let end = self.cursor.next_multiple_of(page).min(self.allocation.len());
        if end > self.finalized {
            // SAFETY: the range lies in the allocation and holds no Rust data.
            unsafe {
                let start = self.base().add(self.finalized);
                region::protect(start, end - self.finalized, region::Protection::READ_EXECUTE)
                    .map_err(|error| ModuleError::Backend(error.into()))?;
            }
        }
        self.finalized = end;
        self.cursor = end;
        Ok(())
    }
}

/// The provider: executable and read-only data share chunks (read-only data may as well be
/// executable); writable data, which blocks never have, goes to leaked heap memory.
#[derive(Default)]
pub(super) struct CodeMemory {
    chunks: Vec<Chunk>,
}

// SAFETY: the chunks are plain memory owned by the provider; the module that owns it moves
// between threads under the `Jit`'s lock, as with Cranelift's own providers.
#[allow(clippy::non_send_fields_in_send_ty, reason = "the allocations are plain memory")]
unsafe impl Send for CodeMemory {}

impl JITMemoryProvider for CodeMemory {
    fn allocate(&mut self, size: usize, align: u64, kind: JITMemoryKind) -> io::Result<*mut u8> {
        let align = usize::try_from(align).unwrap_or(16).max(1);
        if matches!(kind, JITMemoryKind::Writable) {
            let layout = std::alloc::Layout::from_size_align(size.max(1), align)
                .map_err(io::Error::other)?;
            // SAFETY: the layout has a non-zero size. The memory is never freed, as the
            // code that uses it lives as long as the process may run it.
            let pointer = unsafe { std::alloc::alloc_zeroed(layout) };
            return if pointer.is_null() {
                Err(io::ErrorKind::OutOfMemory.into())
            } else {
                Ok(pointer)
            };
        }
        if let Some(pointer) = self.chunks.last_mut().and_then(|chunk| chunk.allocate(size, align))
        {
            return Ok(pointer);
        }
        // What the full chunk got is executable already, or becomes so on finalizing.
        let mut chunk = Chunk::new(CHUNK_SIZE.max(size.next_multiple_of(region::page::size())))?;
        let pointer = chunk.allocate(size, align).ok_or(io::ErrorKind::OutOfMemory)?;
        self.chunks.push(chunk);
        Ok(pointer)
    }

    unsafe fn free_memory(&mut self) {
        for mut chunk in self.chunks.drain(..) {
            // SAFETY: the caller guarantees that no code in the chunk runs any more.
            unsafe { ManuallyDrop::drop(&mut chunk.allocation) };
        }
    }

    fn finalize(&mut self, _branch_protection: BranchProtection) -> ModuleResult<()> {
        for chunk in &mut self.chunks {
            chunk.finalize()?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use cranelift_jit::{BranchProtection, JITMemoryKind, JITMemoryProvider};

    use super::CodeMemory;

    /// Intent: translated blocks are packed into one chunk one after another, aligned as
    /// asked, instead of one mapping per block (the fix that took gigabytes off BotW).
    /// Pass: two executable allocations are 16-byte aligned and the second follows the
    /// first in the same chunk; finalizing succeeds; writable data comes from the heap.
    #[test]
    fn packs_blocks_into_one_chunk() {
        let mut memory = CodeMemory::default();
        let first = memory.allocate(40, 16, JITMemoryKind::Executable).unwrap() as usize;
        let second = memory.allocate(24, 16, JITMemoryKind::Executable).unwrap() as usize;
        assert_eq!(first % 16, 0);
        assert_eq!(second, first + 48);
        assert_eq!(memory.chunks.len(), 1);
        memory.finalize(BranchProtection::None).unwrap();
        let data = memory.allocate(8, 8, JITMemoryKind::Writable).unwrap();
        assert!(!data.is_null());
        // SAFETY: no code in the chunks runs.
        unsafe { memory.free_memory() };
        assert!(memory.chunks.is_empty());
    }
}
