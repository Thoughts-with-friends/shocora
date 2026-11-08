//! Frame heap (`MEMFrmHeap`): bump allocation from both ends, freed in bulk.

/// Which end(s) `MEMFreeToFrmHeap` releases.
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub struct FreeMode(pub u32);

impl FreeMode {
    pub const HEAD: Self = Self(1);
    pub const TAIL: Self = Self(2);

    const fn has(self, other: Self) -> bool {
        self.0 & other.0 != 0
    }
}

/// A snapshot made by `MEMRecordStateForFrmHeap`.
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
struct RecordedState {
    id: u32,
    head: u32,
    tail: u32,
}

/// Host-side bookkeeping of one frame heap.
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct FrmHeap {
    start: u32,
    end: u32,
    head: u32,
    tail: u32,
    /// Last head allocation, for `MEMResizeForMBlockFrmHeap`.
    last_head_block: Option<u32>,
    states: Vec<RecordedState>,
}

const fn alignment(align: i32) -> u32 {
    let a = align.unsigned_abs();
    if a < 4 { 4 } else { a.next_power_of_two() }
}

impl FrmHeap {
    #[inline]
    pub const fn new(start: u32, size: u32) -> Self {
        let start = start.next_multiple_of(4);
        let end = (start.saturating_add(size)) & !3;
        Self { start, end, head: start, tail: end, last_head_block: None, states: Vec::new() }
    }

    #[inline]
    pub const fn start(&self) -> u32 {
        self.start
    }

    #[inline]
    pub const fn end(&self) -> u32 {
        self.end
    }

    #[inline]
    pub const fn contains(&self, address: u32) -> bool {
        address >= self.start && address < self.end
    }

    /// Allocates from the head (`align > 0`) or the tail (`align < 0`).
    pub fn alloc(&mut self, size: u32, align: i32) -> Option<u32> {
        let a = alignment(align);
        let size = size.max(1).next_multiple_of(4);
        if align >= 0 {
            let address = self.head.checked_next_multiple_of(a)?;
            let end = address.checked_add(size)?;
            if end > self.tail {
                return None;
            }
            self.head = end;
            self.last_head_block = Some(address);
            Some(address)
        } else {
            let address = self.tail.checked_sub(size)? & !(a - 1);
            if address < self.head {
                return None;
            }
            self.tail = address;
            Some(address)
        }
    }

    /// Releases every allocation at the selected end(s).
    pub fn free(&mut self, mode: FreeMode) {
        if mode.has(FreeMode::HEAD) {
            self.head = self.start;
            self.last_head_block = None;
        }
        if mode.has(FreeMode::TAIL) {
            self.tail = self.end;
        }
        self.states.clear();
    }

    /// Largest head allocation possible with `align`.
    pub fn allocatable_size(&self, align: i32) -> u32 {
        let a = alignment(align);
        self.head
            .checked_next_multiple_of(a)
            .and_then(|start| self.tail.checked_sub(start))
            .unwrap_or(0)
    }

    #[inline]
    pub fn record_state(&mut self, id: u32) {
        self.states.push(RecordedState { id, head: self.head, tail: self.tail });
    }

    /// Restores the newest state (`id == 0`) or the newest state tagged `id`.
    pub fn free_by_state(&mut self, id: u32) -> bool {
        let Some(index) = self.states.iter().rposition(|s| id == 0 || s.id == id) else {
            return false;
        };
        let state = self.states[index];
        self.states.truncate(index);
        self.head = state.head;
        self.tail = state.tail;
        self.last_head_block = None;
        true
    }

    /// Shrinks the heap to its head allocations; returns the new heap size.
    pub fn adjust(&mut self) -> u32 {
        if self.tail != self.end {
            return 0;
        }
        self.end = self.head;
        self.tail = self.head;
        self.end - self.start
    }

    /// Resizes the most recent head block in place; returns the new size or 0.
    pub fn resize_block(&mut self, address: u32, size: u32) -> u32 {
        if self.last_head_block != Some(address) {
            return 0;
        }
        let size = size.max(1).next_multiple_of(4);
        match address.checked_add(size) {
            Some(end) if end <= self.tail => {
                self.head = end;
                size
            }
            _ => 0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn head_tail_and_states() {
        let mut heap = FrmHeap::new(0x1000, 0x1000);
        assert_eq!(heap.alloc(0x10, 4), Some(0x1000));
        assert_eq!(heap.alloc(0x10, 0x100), Some(0x1100));
        assert_eq!(heap.resize_block(0x1100, 0x40), 0x40);
        assert_eq!(heap.resize_block(0x1000, 0x40), 0, "only the last head block can grow");
        assert_eq!(heap.alloc(0x20, -0x20), Some(0x1FE0));
        assert_eq!(heap.allocatable_size(4), 0x1FE0 - 0x1140);
        heap.record_state(7);
        heap.alloc(0x100, 4).unwrap();
        assert!(heap.free_by_state(7));
        assert_eq!(heap.allocatable_size(4), 0x1FE0 - 0x1140);
        heap.free(FreeMode(3));
        assert_eq!(heap.allocatable_size(4), 0x1000);
        assert_eq!(heap.alloc(0x2000, 4), None);
    }

    #[test]
    fn adjust_shrinks_to_head() {
        let mut heap = FrmHeap::new(0x1000, 0x1000);
        heap.alloc(0x100, 4).unwrap();
        assert_eq!(heap.adjust(), 0x100);
        assert_eq!(heap.alloc(4, 4), None);
    }
}
