//! A first-fit heap over a guest address range (the `MEMExpHeap` model).
use std::collections::BTreeMap;

/// Free blocks plus live allocations of one heap.
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct ExpHeap {
    start: u32,
    end: u32,
    /// Free blocks, address → size, never adjacent.
    free: BTreeMap<u32, u32>,
    /// Live allocations, address → size.
    used: BTreeMap<u32, u32>,
}

impl ExpHeap {
    pub fn new(start: u32, size: u32) -> Self {
        let start_aligned = start.next_multiple_of(4);
        let end = start.saturating_add(size) & !3;
        let mut free = BTreeMap::new();
        if end > start_aligned {
            free.insert(start_aligned, end - start_aligned);
        }
        Self { start: start_aligned, end, free, used: BTreeMap::new() }
    }

    #[inline]
    pub const fn start(&self) -> u32 {
        self.start
    }

    #[inline]
    pub const fn end(&self) -> u32 {
        self.end
    }

    fn normalise_align(align: i32) -> u32 {
        align.unsigned_abs().max(4).next_power_of_two()
    }

    /// Allocates `size` bytes. A negative `align` allocates from the top of
    /// the heap, as `MEMAllocFromExpHeapEx` does.
    pub fn alloc(&mut self, size: u32, align: i32) -> Option<u32> {
        let size = size.max(1).next_multiple_of(4);
        let a = Self::normalise_align(align);
        let fits = |(block, block_size): (&u32, &u32)| -> Option<(u32, u32, u32)> {
            let block_end = block + block_size;
            let address = if align < 0 {
                block_end.checked_sub(size)? & !(a - 1)
            } else {
                block.checked_next_multiple_of(a)?
            };
            (address >= *block && address.checked_add(size)? <= block_end).then_some((
                *block,
                *block_size,
                address,
            ))
        };
        let found = if align < 0 {
            self.free.iter().rev().find_map(fits)
        } else {
            self.free.iter().find_map(fits)
        };
        let (block, block_size, address) = found?;

        self.free.remove(&block);
        if address > block {
            self.free.insert(block, address - block);
        }
        let tail = block + block_size - (address + size);
        if tail > 0 {
            self.free.insert(address + size, tail);
        }
        self.used.insert(address, size);
        Some(address)
    }

    /// Frees an allocation; returns `false` for unknown addresses.
    pub fn free(&mut self, address: u32) -> bool {
        let Some(mut size) = self.used.remove(&address) else { return false };
        let mut start = address;
        if let Some((&prev, &prev_size)) = self.free.range(..address).next_back() {
            if prev + prev_size == address {
                self.free.remove(&prev);
                start = prev;
                size += prev_size;
            }
        }
        if let Some(next_size) = self.free.remove(&(start + size)) {
            size += next_size;
        }
        self.free.insert(start, size);
        true
    }

    /// Size of the largest allocation possible with `align`.
    pub fn allocatable_size(&self, align: i32) -> u32 {
        let a = Self::normalise_align(align);
        self.free
            .iter()
            .filter_map(|(&block, &size)| {
                let aligned = block.checked_next_multiple_of(a)?;
                (block + size).checked_sub(aligned)
            })
            .max()
            .unwrap_or(0)
            & !3
    }

    #[inline]
    pub fn total_free(&self) -> u32 {
        self.free.values().sum()
    }

    #[inline]
    pub fn allocation_size(&self, address: u32) -> Option<u32> {
        self.used.get(&address).copied()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn alloc_align_and_coalesce() {
        let mut heap = ExpHeap::new(0x1000, 0x1000);
        let a = heap.alloc(10, 4).unwrap();
        let b = heap.alloc(0x100, 0x100).unwrap();
        assert_eq!(a, 0x1000);
        assert_eq!(b, 0x1100);
        let top = heap.alloc(0x10, -0x20).unwrap();
        assert_eq!(top, 0x1FE0);
        assert!(heap.free(a));
        assert!(!heap.free(a));
        assert!(heap.free(b));
        assert!(heap.free(top));
        assert_eq!(heap.total_free(), 0x1000);
        assert_eq!(heap.allocatable_size(4), 0x1000);
    }

    #[test]
    fn exhaustion() {
        let mut heap = ExpHeap::new(0x1000, 0x100);
        assert!(heap.alloc(0x100, 4).is_some());
        assert!(heap.alloc(4, 4).is_none());
        assert_eq!(heap.allocatable_size(4), 0);
    }
}
