//! ## Where this file sits
//!
//! ```text
//! Game files ─► Loader ─► CPU ◄─► Memory ◄─► OS (HLE) ─► GPU ─► Screen
//!                                 ▲▲▲▲▲▲  here
//! Controllers, clock ─► OS (HLE) ─► Sound ─► Speakers
//!
//! ┌────────────────────┐   ┌──────────────────────────┐   ┌────────────────────┐
//! │ bad access         │   │ error.rs                 │   │ error messages     │
//! │                    │──►│ ──────────────────────── │──►│                    │
//! │                    │   │ memory errors            │   │                    │
//! └────────────────────┘   └──────────────────────────┘   └────────────────────┘
//! ```
//!
//! What goes wrong when something touches memory that does not exist. Terms such as HLE, JIT or
//! shader are explained in `docs/guide/wiiu_term.md`.

/// A guest memory access that could not be served.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum MemoryError {
    #[error("unmapped guest access at {address:#010x} (+{len} bytes)")]
    Unmapped { address: u32, len: u32 },
    #[error("region {name} at {start:#010x} overlaps an existing mapping")]
    Overlap { name: &'static str, start: u32 },
}

#[cfg(test)]
mod tests {
    use super::MemoryError;

    /// Intent: an unmapped access reports the guest address and size, the first clue in
    /// any guest crash.
    /// Pass: the address is printed as 8 hex digits with the access length.
    #[test]
    fn unmapped_access_shows_address_and_length() {
        let error = MemoryError::Unmapped { address: 0x1000_0000, len: 4 };
        assert_eq!(error.to_string(), "unmapped guest access at 0x10000000 (+4 bytes)");
        let overlap = MemoryError::Overlap { name: "MEM2", start: 0x1000_0000 };
        assert!(overlap.to_string().contains("MEM2"));
    }
}
