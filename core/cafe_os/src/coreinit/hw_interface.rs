//! Direct access to hardware registers (port of Cemu's coreinit_HWInterface and SI).
//!
//! Titles with a GameCube heritage bring a pad library that talks to the serial
//! interface itself. No GameCube controller is ever attached here: every polled
//! channel answers "no response". A register reading as zero would say the opposite,
//! and a controller in port 3 is what such titles take for a developer's.

use wiiu_hle_macros::hle_export;

use crate::hle::HleContext;

/// Register group of `__OSReadRegister32Ex`: the serial interface.
const INTERFACE_SI: u32 = 6;

/// Word index of the serial interface's registers.
mod si {
    /// `SICnOUTBUF` is the first of each channel's three registers.
    pub const OUT_BUFFERS: u32 = 12;
    pub const CHANNEL_REGISTERS: u32 = 3;
    pub const POLL: u32 = 12;
    pub const COMMUNICATION: u32 = 13;
    pub const STATUS: u32 = 14;
}

/// `SICOMCSR`: transfer complete, cleared by writing it.
const COMMUNICATION_INTERRUPT: u32 = 1 << 31;
/// `SICOMCSR`: starts a transfer, which here ends at once.
const COMMUNICATION_START: u32 = 1;
/// `SIPOLL`: lowest of the four channel enable bits, channel 3's.
const POLL_ENABLE_SHIFT: u32 = 4;

/// `SISR`: no response on `channel`.
const fn no_response_bit(channel: u32) -> u32 {
    1 << (27 - 8 * channel)
}

/// The serial interface's registers.
#[derive(serde::Serialize, serde::Deserialize, Debug, Default)]
pub struct SerialInterface {
    out_buffers: [u32; 4],
    poll: u32,
    communication: u32,
    /// Channels that found nothing to talk to, as a bit mask.
    no_response: u32,
}

impl SerialInterface {
    fn read(&mut self, register: u32) -> u32 {
        match register {
            si::POLL => self.poll,
            si::COMMUNICATION => self.communication,
            si::STATUS => {
                // Polling an empty port fails, and all ports are empty.
                let enabled = (self.poll >> POLL_ENABLE_SHIFT) & 0xF;
                for channel in 0..4 {
                    if (enabled >> (3 - channel)) & 1 != 0 {
                        self.no_response |= 1 << channel;
                    }
                }
                (0..4)
                    .filter(|channel| self.no_response & (1 << channel) != 0)
                    .fold(0, |status, channel| status | no_response_bit(channel))
            }
            register
                if register < si::OUT_BUFFERS && register.is_multiple_of(si::CHANNEL_REGISTERS) =>
            {
                self.out_buffers[(register / si::CHANNEL_REGISTERS) as usize]
            }
            // The input buffers: nothing ever arrives.
            _ => 0,
        }
    }

    fn write(&mut self, register: u32, value: u32) {
        match register {
            si::POLL => self.poll = value,
            si::COMMUNICATION => {
                self.communication = value & !(COMMUNICATION_INTERRUPT | COMMUNICATION_START);
            }
            si::STATUS => {
                for channel in 0..4 {
                    if value & no_response_bit(channel) != 0 {
                        self.no_response &= !(1 << channel);
                    }
                }
            }
            register
                if register < si::OUT_BUFFERS && register.is_multiple_of(si::CHANNEL_REGISTERS) =>
            {
                self.out_buffers[(register / si::CHANNEL_REGISTERS) as usize] = value;
            }
            _ => {}
        }
    }
}

#[hle_export(lib = "coreinit", name = "__OSReadRegister32Ex")]
fn os_read_register_32_ex(ctx: &mut HleContext<'_>, interface: u32, register: u32) -> u32 {
    if interface == INTERFACE_SI { ctx.os.coreinit.serial_interface.read(register) } else { 0 }
}

#[hle_export(lib = "coreinit", name = "__OSWriteRegister32Ex")]
fn os_write_register_32_ex(ctx: &mut HleContext<'_>, interface: u32, register: u32, value: u32) {
    if interface == INTERFACE_SI {
        ctx.os.coreinit.serial_interface.write(register, value);
    }
}

/// The video interface: nothing a title gets from it matters here.
#[hle_export(lib = "coreinit", name = "OSReadRegister16")]
fn os_read_register_16(_ctx: &mut HleContext<'_>, _interface: u32, _offset: u32) -> u32 {
    0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hle::test_env::TestEnv;

    const READ: &str = "__OSReadRegister32Ex";
    const WRITE: &str = "__OSWriteRegister32Ex";

    #[test]
    fn polled_channels_report_no_controller_until_acknowledged() {
        let mut env = TestEnv::new();
        let status = |env: &mut TestEnv| env.call("coreinit", READ, &[INTERFACE_SI, si::STATUS]);
        assert_eq!(status(&mut env), 0, "nothing polled yet");

        // Channels 0 and 2: the enable bits count from the top.
        env.call("coreinit", WRITE, &[INTERFACE_SI, si::POLL, 0b1010 << POLL_ENABLE_SHIFT]);
        assert_eq!(status(&mut env), no_response_bit(0) | no_response_bit(2));

        env.call("coreinit", WRITE, &[INTERFACE_SI, si::POLL, 0]);
        env.call("coreinit", WRITE, &[INTERFACE_SI, si::STATUS, no_response_bit(0)]);
        assert_eq!(status(&mut env), no_response_bit(2));
    }

    #[test]
    fn registers_of_the_interface_keep_what_is_written() {
        let mut env = TestEnv::new();
        // Channel 1's output buffer is the fourth register.
        env.call("coreinit", WRITE, &[INTERFACE_SI, 3, 0x0040_0300]);
        assert_eq!(env.call("coreinit", READ, &[INTERFACE_SI, 3]), 0x0040_0300);
        // Its input buffers stay empty.
        assert_eq!(env.call("coreinit", READ, &[INTERFACE_SI, 4]), 0);
        // A transfer is over as soon as it starts.
        env.call("coreinit", WRITE, &[INTERFACE_SI, si::COMMUNICATION, 0x8000_0101]);
        assert_eq!(env.call("coreinit", READ, &[INTERFACE_SI, si::COMMUNICATION]), 0x100);
        // Other register groups read as nothing.
        assert_eq!(env.call("coreinit", READ, &[3, 3]), 0);
    }
}
