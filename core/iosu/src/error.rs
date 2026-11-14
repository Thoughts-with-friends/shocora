#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum IosError {
    #[error("{0}: no such IOSU device")]
    NoSuchDevice(String),
    #[error("device handle {0} is not open")]
    BadHandle(u32),
}

#[cfg(test)]
mod tests {
    use super::IosError;

    /// Intent: IOSU errors name the device or handle a title used wrongly.
    /// Pass: the device name and the handle number appear in the messages.
    #[test]
    fn messages_name_device_and_handle() {
        assert_eq!(
            IosError::NoSuchDevice("/dev/x".into()).to_string(),
            "/dev/x: no such IOSU device"
        );
        assert_eq!(IosError::BadHandle(3).to_string(), "device handle 3 is not open");
    }
}
