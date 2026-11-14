use wiiu_memory::MemoryError;

#[derive(Debug, thiserror::Error)]
pub enum GpuError {
    #[error("PM4 packet at word {index} is truncated")]
    TruncatedPacket { index: usize },
    #[error("indirect buffers nested deeper than {0}")]
    IndirectTooDeep(u32),
    #[error("physical address {0:#010x} is not backed by MEM1/MEM2")]
    BadPhysicalAddress(u32),
    #[error("the GPU thread stopped")]
    Stopped,
    #[error("the GPU thread panicked: {0}")]
    Panicked(String),
    #[error(transparent)]
    Memory(#[from] MemoryError),
}

#[cfg(test)]
mod tests {
    use super::GpuError;

    /// Intent: GPU errors say where a command stream went wrong.
    /// Pass: the packet index and the physical address appear in the messages, and a
    /// memory error converts with `?`.
    #[test]
    fn messages_locate_the_fault() {
        assert!(GpuError::TruncatedPacket { index: 7 }.to_string().contains("word 7"));
        assert!(GpuError::BadPhysicalAddress(0x1234).to_string().contains("0x00001234"));
        let memory = wiiu_memory::MemoryError::Unmapped { address: 4, len: 4 };
        assert!(matches!(GpuError::from(memory), GpuError::Memory(_)));
    }
}
