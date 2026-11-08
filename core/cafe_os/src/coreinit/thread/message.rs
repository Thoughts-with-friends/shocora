//! `OSMessageQueue`: a ring buffer of 16-byte messages in guest memory.
use wiiu_hle_macros::hle_export;

use super::{Reschedule, Wait, block};
use crate::hle::{HleContext, HleError};

mod offset {
    pub const USER_DATA: u32 = 0x04;
    pub const ARRAY: u32 = 0x2C;
    pub const COUNT: u32 = 0x30;
    pub const FIRST: u32 = 0x34;
    pub const USED: u32 = 0x38;
}

/// `sizeof(OSMessageQueue)`.
const QUEUE_SIZE: u32 = 0x3C;
/// `sizeof(OSMessage)`.
const MESSAGE_SIZE: u32 = 0x10;
/// `"mSgQ"`.
const QUEUE_MAGIC: u32 = 0x6D53_6751;
/// `OS_MESSAGE_BLOCK`.
const FLAG_BLOCK: u32 = 1;
/// Slots of the system message queue.
const SYSTEM_QUEUE_SLOTS: u32 = 16;

/// The ring-buffer fields of a queue.
#[derive(serde::Serialize, serde::Deserialize)]
struct Ring {
    array: u32,
    count: u32,
    first: u32,
    used: u32,
}

impl Ring {
    fn read(ctx: &HleContext<'_>, queue: u32) -> Result<Self, HleError> {
        let field = |o| ctx.memory.read_u32(queue + o);
        Ok(Self {
            array: field(offset::ARRAY)?,
            count: field(offset::COUNT)?,
            first: field(offset::FIRST)?,
            used: field(offset::USED)?,
        })
    }

    fn write(&self, ctx: &mut HleContext<'_>, queue: u32) -> Result<(), HleError> {
        ctx.memory.write_u32(queue + offset::FIRST, self.first)?;
        ctx.memory.write_u32(queue + offset::USED, self.used)?;
        Ok(())
    }

    const fn slot(&self, index: u32) -> u32 {
        self.array + (index % self.count) * MESSAGE_SIZE
    }
}

fn init(
    ctx: &mut HleContext<'_>,
    queue: u32,
    array: u32,
    count: u32,
    user_data: u32,
) -> Result<(), HleError> {
    ctx.memory.fill(queue, QUEUE_SIZE, 0)?;
    ctx.memory.write_u32(queue, QUEUE_MAGIC)?;
    ctx.memory.write_u32(queue + offset::USER_DATA, user_data)?;
    ctx.memory.write_u32(queue + offset::ARRAY, array)?;
    ctx.memory.write_u32(queue + offset::COUNT, count)?;
    Ok(())
}

pub(crate) fn send(
    ctx: &mut HleContext<'_>,
    queue: u32,
    message: u32,
    flags: u32,
    jam: bool,
) -> Result<bool, HleError> {
    let mut ring = Ring::read(ctx, queue)?;
    if ring.count == 0 || ring.used >= ring.count {
        if flags & FLAG_BLOCK != 0 {
            block(ctx, Wait::MessageSend(queue));
        }
        return Ok(false);
    }
    let index = if jam {
        ring.first = (ring.first + ring.count - 1) % ring.count;
        ring.first
    } else {
        ring.first + ring.used
    };
    ctx.memory.copy_within(ring.slot(index), message, MESSAGE_SIZE)?;
    ring.used += 1;
    ring.write(ctx, queue)?;
    ctx.os.coreinit.threads.request(Reschedule::Priority);
    Ok(true)
}

fn receive(
    ctx: &mut HleContext<'_>,
    queue: u32,
    message: u32,
    flags: u32,
    remove: bool,
) -> Result<bool, HleError> {
    let mut ring = Ring::read(ctx, queue)?;
    if ring.count == 0 || ring.used == 0 {
        if remove && flags & FLAG_BLOCK != 0 {
            block(ctx, Wait::MessageReceive(queue));
        }
        return Ok(false);
    }
    if message != 0 {
        ctx.memory.copy_within(message, ring.slot(ring.first), MESSAGE_SIZE)?;
    }
    if remove {
        ring.first = (ring.first + 1) % ring.count;
        ring.used -= 1;
        ring.write(ctx, queue)?;
        ctx.os.coreinit.threads.request(Reschedule::Priority);
    }
    Ok(true)
}

/// Thread queues are kept host-side; only the guest header is cleared.
#[hle_export(lib = "coreinit", name = "OSInitThreadQueue")]
fn os_init_thread_queue(ctx: &mut HleContext<'_>, queue: u32) -> Result<(), HleError> {
    Ok(ctx.memory.fill(queue, 0x10, 0)?)
}

#[hle_export(lib = "coreinit", name = "OSInitThreadQueueEx")]
fn os_init_thread_queue_ex(
    ctx: &mut HleContext<'_>,
    queue: u32,
    user_data: u32,
) -> Result<(), HleError> {
    ctx.memory.fill(queue, 0x10, 0)?;
    Ok(ctx.memory.write_u32(queue + 8, user_data)?)
}

#[hle_export(lib = "coreinit", name = "OSInitMessageQueue")]
fn os_init_message_queue(
    ctx: &mut HleContext<'_>,
    queue: u32,
    array: u32,
    count: u32,
) -> Result<(), HleError> {
    init(ctx, queue, array, count, 0)
}

#[hle_export(lib = "coreinit", name = "OSInitMessageQueueEx")]
fn os_init_message_queue_ex(
    ctx: &mut HleContext<'_>,
    queue: u32,
    array: u32,
    count: u32,
    user_data: u32,
) -> Result<(), HleError> {
    init(ctx, queue, array, count, user_data)
}

#[hle_export(lib = "coreinit", name = "OSSendMessage")]
fn os_send_message(
    ctx: &mut HleContext<'_>,
    queue: u32,
    message: u32,
    flags: u32,
) -> Result<bool, HleError> {
    send(ctx, queue, message, flags, false)
}

/// Like `OSSendMessage`, but the message goes to the front.
#[hle_export(lib = "coreinit", name = "OSJamMessage")]
fn os_jam_message(
    ctx: &mut HleContext<'_>,
    queue: u32,
    message: u32,
    flags: u32,
) -> Result<bool, HleError> {
    send(ctx, queue, message, flags, true)
}

#[hle_export(lib = "coreinit", name = "OSReceiveMessage")]
fn os_receive_message(
    ctx: &mut HleContext<'_>,
    queue: u32,
    message: u32,
    flags: u32,
) -> Result<bool, HleError> {
    receive(ctx, queue, message, flags, true)
}

#[hle_export(lib = "coreinit", name = "OSPeekMessage")]
fn os_peek_message(ctx: &mut HleContext<'_>, queue: u32, message: u32) -> Result<bool, HleError> {
    receive(ctx, queue, message, 0, false)
}

/// The queue the OS posts system messages to (created on first use).
#[hle_export(lib = "coreinit", name = "OSGetSystemMessageQueue")]
fn os_get_system_message_queue(ctx: &mut HleContext<'_>) -> Result<u32, HleError> {
    if ctx.os.coreinit.system_message_queue == 0 {
        let queue = ctx.os.registry.allocate_data(QUEUE_SIZE, 8);
        let array = ctx.os.registry.allocate_data(SYSTEM_QUEUE_SLOTS * MESSAGE_SIZE, 8);
        init(ctx, queue, array, SYSTEM_QUEUE_SLOTS, 0)?;
        ctx.os.coreinit.system_message_queue = queue;
    }
    Ok(ctx.os.coreinit.system_message_queue)
}

#[cfg(test)]
mod tests {
    use crate::hle::test_env::{SCRATCH, TestEnv};

    #[test]
    fn fifo_jam_and_full_queue() {
        let mut env = TestEnv::new();
        let (queue, array, msg) = (SCRATCH, SCRATCH + 0x100, SCRATCH + 0x200);
        env.call("coreinit", "OSInitMessageQueue", &[queue, array, 2]);
        for value in [1, 2, 3] {
            env.memory.write_u32(msg, value).unwrap();
            let sent = env.call("coreinit", "OSSendMessage", &[queue, msg, 0]);
            assert_eq!(sent, u32::from(value != 3), "third send overflows a 2-slot queue");
        }
        env.call("coreinit", "OSReceiveMessage", &[queue, msg, 0]);
        assert_eq!(env.memory.read_u32(msg).unwrap(), 1);
        env.memory.write_u32(msg, 9).unwrap();
        assert_eq!(env.call("coreinit", "OSJamMessage", &[queue, msg, 0]), 1);
        env.call("coreinit", "OSReceiveMessage", &[queue, msg, 0]);
        assert_eq!(env.memory.read_u32(msg).unwrap(), 9);
        env.call("coreinit", "OSReceiveMessage", &[queue, msg, 0]);
        assert_eq!(env.memory.read_u32(msg).unwrap(), 2);
        assert_eq!(env.call("coreinit", "OSReceiveMessage", &[queue, msg, 0]), 0);
    }
}
