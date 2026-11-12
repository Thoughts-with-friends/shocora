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
