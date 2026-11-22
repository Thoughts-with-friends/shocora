//! ## Where this file sits
//!
//! ```text
//! Game files ─► Loader ─► CPU ◄─► Memory ◄─► OS (HLE) ─► GPU ─► Screen
//! ▲▲▲▲▲▲▲▲▲▲    ▲▲▲▲▲▲  here
//! Controllers, clock ─► OS (HLE) ─► Sound ─► Speakers
//!
//! ┌────────────────────┐   ┌──────────────────────────┐   ┌────────────────────┐
//! │ path text          │   │ path.rs                  │   │ clean path parts   │
//! │                    │──►│ ──────────────────────── │──►│                    │
//! │                    │   │ guest paths              │   │                    │
//! └────────────────────┘   └──────────────────────────┘   └────────────────────┘
//! ```
//!
//! Splits and cleans up Wii U-style paths. Terms such as HLE, JIT or shader are explained in
//! `docs/guide/wiiu_term.md`.

use crate::FsError;

/// A normalised, absolute guest path: `/`-separated, no `.`/`..`/empty parts.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct GuestPath(String);

impl GuestPath {
    /// # Errors
    /// Fails for relative paths and paths escaping the root with `..`.
    pub fn new(raw: &str) -> Result<Self, FsError> {
        if !raw.starts_with('/') {
            return Err(FsError::InvalidPath(raw.to_owned()));
        }
        let mut parts: Vec<&str> = Vec::new();
        for part in raw.split('/') {
            match part {
                "" | "." => {}
                ".." => {
                    parts.pop().ok_or_else(|| FsError::InvalidPath(raw.to_owned()))?;
                }
                other => parts.push(other),
            }
        }
        Ok(Self(format!("/{}", parts.join("/"))))
    }

    #[inline]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Components after the root.
    #[inline]
    pub fn components(&self) -> impl Iterator<Item = &str> {
        self.0.split('/').filter(|p| !p.is_empty())
    }

    /// Remainder after `prefix`, if `prefix` is a whole-component prefix.
    pub fn strip_prefix(&self, prefix: &Self) -> Option<Self> {
        if prefix.0 == "/" {
            return Some(self.clone());
        }
        let rest = self.0.strip_prefix(&prefix.0)?;
        match rest {
            "" => Some(Self("/".to_owned())),
            r if r.starts_with('/') => Some(Self(r.to_owned())),
            _ => None,
        }
    }
}

impl std::fmt::Display for GuestPath {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalises() {
        assert_eq!(GuestPath::new("/vol//content/./a/../b").unwrap().as_str(), "/vol/content/b");
        assert!(GuestPath::new("/..").is_err());
        assert!(GuestPath::new("vol").is_err());
    }

    #[test]
    fn strips_whole_components() {
        let p = GuestPath::new("/vol/content/x").unwrap();
        let mount = GuestPath::new("/vol/content").unwrap();
        assert_eq!(p.strip_prefix(&mount).unwrap().as_str(), "/x");
        assert_eq!(GuestPath::new("/vol/contentx").unwrap().strip_prefix(&mount), None);
        assert_eq!(mount.strip_prefix(&mount).unwrap().as_str(), "/");
    }
}
