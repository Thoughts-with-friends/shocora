#[derive(Debug, thiserror::Error)]
pub enum FsError {
    #[error("{0}: no such file or directory")]
    NotFound(String),
    #[error("{0}: invalid guest path")]
    InvalidPath(String),
    #[error("{0}: no device mounted")]
    NotMounted(String),
    #[error("{path}: {source}")]
    Io { path: String, source: std::io::Error },
}

#[cfg(test)]
mod tests {
    use super::FsError;

    /// Intent: errors name the guest path they are about, which is what `FS` logs show.
    /// Pass: each message starts with the path and says what went wrong.
    #[test]
    fn messages_name_the_path() {
        assert_eq!(
            FsError::NotFound("/vol/x".into()).to_string(),
            "/vol/x: no such file or directory"
        );
        assert_eq!(FsError::NotMounted("/dev".into()).to_string(), "/dev: no device mounted");
        let io = FsError::Io { path: "/vol/y".into(), source: std::io::ErrorKind::Other.into() };
        assert!(io.to_string().starts_with("/vol/y: "));
    }
}
