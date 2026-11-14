//! The command processor (CP): PM4 parsing and execution.
pub mod executor;
pub mod opcode;
pub mod pm4;

pub use executor::CommandProcessor;
pub use pm4::{Packet, parse};

#[cfg(test)]
mod tests {
    use super::*;

    /// Intent: a PM4 command stream splits into packets; a type-2 filler word is a packet
    /// of its own.
    /// Pass: two filler words give two packets, and an empty stream none.
    #[test]
    fn filler_words_are_packets() {
        assert_eq!(parse(&[0x8000_0000, 0x8000_0000]).count(), 2);
        assert_eq!(parse(&[]).count(), 0);
    }
}
