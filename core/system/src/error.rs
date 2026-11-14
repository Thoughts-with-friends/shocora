use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum EmuError {
    #[error("{0}")]
    State(String),
    #[error(transparent)]
    Title(#[from] wiiu_title::TitleError),
    #[error("{path}: {source}")]
    Io { path: PathBuf, source: std::io::Error },
    #[error(transparent)]
    Boot(#[from] wiiu_cafe_os::BootError),
    #[error(transparent)]
    Memory(#[from] wiiu_memory::MemoryError),
    #[error(transparent)]
    Fs(#[from] wiiu_fs::FsError),
}

#[cfg(test)]
mod tests {
    use super::EmuError;

    /// Intent: errors of the lower layers reach the front end unchanged through `?`.
    /// Pass: a file-system error converts and keeps its message.
    #[test]
    fn lower_errors_convert_with_their_message() {
        let fs = wiiu_fs::FsError::NotFound("/vol/a".into());
        let message = fs.to_string();
        let error = EmuError::from(fs);
        assert!(matches!(error, EmuError::Fs(_)));
        assert_eq!(error.to_string(), message);
    }
}
