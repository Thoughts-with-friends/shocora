//! `MP*`: task queues the game's worker threads pull from.
//!
//! Tasks are guest functions. Instead of a nested interpreter (a task may
//! block), `MPRunTask*` jump into the task with `lr` on an internal stub; the
//! stub records the result and either starts the next task or returns to the
//! original caller.
use std::collections::HashMap;

use wiiu_hle_macros::hle_export;

use crate::{
    hle::{HleContext, HleError},
    process::INTERNAL_LIBRARY,
};

mod task {
    pub const THIS: u32 = 0x00;
    pub const QUEUE: u32 = 0x04;
    pub const STATE: u32 = 0x08;
    pub const FUNC: u32 = 0x0C;
    pub const DATA: u32 = 0x10;
    pub const SIZE: u32 = 0x14;
    pub const RESULT: u32 = 0x18;
    pub const CORE: u32 = 0x1C;
    pub const RUNTIME: u32 = 0x20;
    pub const USER_DATA: u32 = 0x28;
    pub const INFO_SIZE: u32 = 0x18;
}

mod queue {
    pub const THIS: u32 = 0x00;
    pub const STATE: u32 = 0x04;
    pub const TASK_COUNT: u32 = 0x08;
    pub const READY: u32 = 0x0C;
    pub const RUNNING: u32 = 0x10;
    pub const DONE: u32 = 0x14;
    pub const NEXT: u32 = 0x20;
    pub const END: u32 = 0x2C;
    pub const TASKS: u32 = 0x38;
    pub const CAPACITY: u32 = 0x3C;
}

/// `MPTaskState`.
mod task_state {
    pub const INIT: u32 = 1;
    pub const READY: u32 = 2;
    pub const RUN: u32 = 4;
    pub const DONE: u32 = 8;
}

/// `MPTaskQState`.
mod queue_state {
    pub const INIT: u32 = 1;
    pub const RUN: u32 = 2;
    pub const STOPPING: u32 = 4;
    pub const STOP: u32 = 8;
    pub const DONE: u32 = 16;
}

/// Index of the per-core counters used: every task runs on the main core's slot.
const SLOT: u32 = 4;

/// A `MPRunTask*` call in progress on one thread.
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, Copy)]
pub struct RunningTasks {
    /// Queue to keep draining, or `None` for a single `MPRunTask`.
    queue: Option<u32>,
    task: u32,
    caller_lr: u32,
    ran_any: bool,
}

/// Per-thread continuations.
#[derive(serde::Serialize, serde::Deserialize, Debug, Default)]
pub struct MpState {
    running: HashMap<u32, RunningTasks>,
}

#[hle_export(lib = "coreinit", name = "MPInitTask")]
fn mp_init_task(
    ctx: &mut HleContext<'_>,
    address: u32,
    func: u32,
    data: u32,
    size: u32,
) -> Result<(), HleError> {
    for (offset, value) in [
        (task::THIS, address),
        (task::QUEUE, 0),
        (task::STATE, task_state::INIT),
        (task::FUNC, func),
        (task::DATA, data),
        (task::SIZE, size),
        (task::RESULT, 0),
        (task::CORE, 3),
        (task::USER_DATA, 0),
    ] {
        ctx.memory.write_u32(address + offset, value)?;
    }
    ctx.memory.write_u64(address + task::RUNTIME, 0)?;
    Ok(())
}

#[hle_export(lib = "coreinit", name = "MPTermTask")]
fn mp_term_task(_ctx: &mut HleContext<'_>, _task: u32) -> bool {
    true
}

#[hle_export(lib = "coreinit", name = "MPGetTaskInfo")]
fn mp_get_task_info(ctx: &mut HleContext<'_>, address: u32, info: u32) -> Result<bool, HleError> {
    for (i, offset) in [task::STATE, task::RESULT, task::CORE].into_iter().enumerate() {
        ctx.memory.write_u32(info + 4 * i as u32, ctx.memory.read_u32(address + offset)?)?;
    }
    ctx.memory.write_u64(info + 0x10, ctx.memory.read_u64(address + task::RUNTIME)?)?;
    debug_assert_eq!(task::INFO_SIZE, 0x18);
    Ok(true)
}

#[hle_export(lib = "coreinit", name = "MPGetTaskUserData")]
fn mp_get_task_user_data(ctx: &mut HleContext<'_>, address: u32) -> Result<u32, HleError> {
    Ok(ctx.memory.read_u32(address + task::USER_DATA)?)
}

#[hle_export(lib = "coreinit", name = "MPSetTaskUserData")]
fn mp_set_task_user_data(
    ctx: &mut HleContext<'_>,
    address: u32,
    data: u32,
) -> Result<(), HleError> {
    Ok(ctx.memory.write_u32(address + task::USER_DATA, data)?)
}

#[hle_export(lib = "coreinit", name = "MPInitTaskQ")]
fn mp_init_task_q(
    ctx: &mut HleContext<'_>,
    address: u32,
    tasks: u32,
    capacity: u32,
) -> Result<(), HleError> {
    ctx.memory.fill(address, 0x50, 0)?;
    ctx.memory.write_u32(address + queue::THIS, address)?;
    ctx.memory.write_u32(address + queue::STATE, queue_state::INIT)?;
    ctx.memory.write_u32(address + queue::TASKS, tasks)?;
    ctx.memory.write_u32(address + queue::CAPACITY, capacity)?;
    Ok(())
}

/// Stops the queue and waits for the tasks still running, whose caller frees them next.
#[hle_export(lib = "coreinit", name = "MPTermTaskQ")]
fn mp_term_task_q(ctx: &mut HleContext<'_>, address: u32) -> Result<bool, HleError> {
    if ctx.memory.read_u32(address + queue::STATE)? == queue_state::RUN {
        ctx.memory.write_u32(address + queue::STATE, queue_state::STOP)?;
    }
    if ctx.memory.read_u32(address + queue::RUNNING)? != 0 {
        ctx.cpu.pc = ctx.cpu.pc.wrapping_sub(4);
        ctx.os.coreinit.threads.request(super::thread::Reschedule::Yield);
        return Ok(false);
    }
    Ok(true)
}

/// Rewinds the queue so its tasks can run again.
#[hle_export(lib = "coreinit", name = "MPResetTaskQ")]
fn mp_reset_task_q(ctx: &mut HleContext<'_>, address: u32) -> Result<bool, HleError> {
    let state = ctx.memory.read_u32(address + queue::STATE)?;
    if !matches!(state, queue_state::STOP | queue_state::DONE) {
        return Ok(false);
    }
    let count = ctx.memory.read_u32(address + queue::END + SLOT)?;
    ctx.memory.write_u32(address + queue::TASK_COUNT, count)?;
    let tasks = ctx.memory.read_u32(address + queue::TASKS)?;
    for i in 0..count {
        let entry = ctx.memory.read_u32(tasks + 4 * i)?;
        ctx.memory.write_u32(entry + task::STATE, task_state::READY)?;
        ctx.memory.write_u32(entry + task::RESULT, 0)?;
        ctx.memory.write_u32(entry + task::CORE, 3)?;
        ctx.memory.write_u64(entry + task::RUNTIME, 0)?;
    }
    ctx.memory.write_u32(address + queue::STATE, queue_state::INIT)?;
    ctx.memory.write_u32(address + queue::READY, count)?;
    ctx.memory.write_u32(address + queue::RUNNING, 0)?;
    ctx.memory.write_u32(address + queue::DONE + SLOT, 0)?;
    ctx.memory.write_u32(address + queue::NEXT + SLOT, 0)?;
    Ok(true)
}

#[hle_export(lib = "coreinit", name = "MPEnqueTask")]
fn mp_enque_task(ctx: &mut HleContext<'_>, address: u32, entry: u32) -> Result<bool, HleError> {
    if ctx.memory.read_u32(entry + task::STATE)? != task_state::INIT {
        return Ok(false);
    }
    let end = ctx.memory.read_u32(address + queue::END + SLOT)?;
    if end >= ctx.memory.read_u32(address + queue::CAPACITY)? {
        return Ok(false);
    }
    ctx.memory.write_u32(entry + task::QUEUE, address)?;
    ctx.memory.write_u32(entry + task::STATE, task_state::READY)?;
    ctx.memory.write_u32(address + queue::END + SLOT, end + 1)?;
    for counter in [queue::TASK_COUNT, queue::READY] {
        ctx.memory.write_u32(address + counter, ctx.memory.read_u32(address + counter)? + 1)?;
    }
    let tasks = ctx.memory.read_u32(address + queue::TASKS)?;
    ctx.memory.write_u32(tasks + 4 * end, entry)?;
    if ctx.memory.read_u32(address + queue::STATE)? == queue_state::DONE {
        ctx.memory.write_u32(address + queue::STATE, queue_state::RUN)?;
    }
    Ok(true)
}

/// Takes up to `count` tasks without running them.
#[hle_export(lib = "coreinit", name = "MPDequeTasks")]
fn mp_deque_tasks(
    ctx: &mut HleContext<'_>,
    address: u32,
    out: u32,
    count: u32,
) -> Result<u32, HleError> {
    if ctx.memory.read_u32(address + queue::STATE)? != queue_state::RUN {
        return Ok(0);
    }
    let next = ctx.memory.read_u32(address + queue::NEXT + SLOT)?;
    let end = ctx.memory.read_u32(address + queue::END + SLOT)?;
    let taken = count.min(end - next);
    let tasks = ctx.memory.read_u32(address + queue::TASKS)?;
    for i in 0..taken {
        let entry = ctx.memory.read_u32(tasks + 4 * (next + i))?;
        ctx.memory.write_u32(out + 4 * i, entry)?;
    }
    ctx.memory.write_u32(address + queue::NEXT + SLOT, next + taken)?;
    Ok(taken)
}

#[hle_export(lib = "coreinit", name = "MPStartTaskQ")]
fn mp_start_task_q(ctx: &mut HleContext<'_>, address: u32) -> Result<bool, HleError> {
    let state = ctx.memory.read_u32(address + queue::STATE)?;
    if !matches!(state, queue_state::INIT | queue_state::STOP) {
        return Ok(false);
    }
    ctx.memory.write_u32(address + queue::STATE, queue_state::RUN)?;
    Ok(true)
}

#[hle_export(lib = "coreinit", name = "MPStopTaskQ")]
fn mp_stop_task_q(ctx: &mut HleContext<'_>, address: u32) -> Result<bool, HleError> {
    if ctx.memory.read_u32(address + queue::STATE)? != queue_state::RUN {
        return Ok(false);
    }
    let running = ctx.memory.read_u32(address + queue::RUNNING)?;
    let state = if running == 0 { queue_state::STOP } else { queue_state::STOPPING };
    ctx.memory.write_u32(address + queue::STATE, state)?;
    Ok(true)
}

#[hle_export(lib = "coreinit", name = "MPGetTaskQInfo")]
fn mp_get_task_q_info(ctx: &mut HleContext<'_>, address: u32, info: u32) -> Result<bool, HleError> {
    let done = ctx.memory.read_u32(address + queue::DONE + SLOT)?;
    for (i, value) in [
        ctx.memory.read_u32(address + queue::STATE)?,
        ctx.memory.read_u32(address + queue::TASK_COUNT)?,
        ctx.memory.read_u32(address + queue::READY)?,
        ctx.memory.read_u32(address + queue::RUNNING)?,
        done,
    ]
    .into_iter()
    .enumerate()
    {
        ctx.memory.write_u32(info + 4 * i as u32, value)?;
    }
    Ok(true)
}

/// Blocks until the queue reaches a state in `mask`: the worker threads make
/// progress while the caller yields.
#[hle_export(lib = "coreinit", name = "MPWaitTaskQ")]
fn mp_wait_task_q(ctx: &mut HleContext<'_>, address: u32, mask: u32) -> Result<bool, HleError> {
    let state = ctx.memory.read_u32(address + queue::STATE)?;
    if state & mask == 0 {
        ctx.cpu.pc = ctx.cpu.pc.wrapping_sub(4);
        ctx.os.coreinit.threads.request(super::thread::Reschedule::Yield);
        return Ok(false);
    }
    Ok(true)
}

fn return_stub(ctx: &mut HleContext<'_>) -> Result<u32, HleError> {
    let stub = ctx.os.registry.function_stub(INTERNAL_LIBRARY, MP_TASK_RETURN.name);
    ctx.os.registry.commit(ctx.memory)?;
    Ok(stub)
}

/// Enters `entry` with the continuation stub as return address.
fn enter_task(ctx: &mut HleContext<'_>, entry: u32, stub: u32) -> Result<(), HleError> {
    ctx.memory.write_u32(entry + task::STATE, task_state::RUN)?;
    ctx.memory.write_u32(entry + task::CORE, ctx.cpu.core_id())?;
    ctx.cpu.pc = ctx.memory.read_u32(entry + task::FUNC)?;
    ctx.cpu.lr = stub;
    ctx.cpu.gpr[3] = ctx.memory.read_u32(entry + task::DATA)?;
    ctx.cpu.gpr[4] = ctx.memory.read_u32(entry + task::SIZE)?;
    Ok(())
}

/// Starts the queue's next task, or returns `None` when there is nothing to do.
fn next_task(ctx: &mut HleContext<'_>, address: u32) -> Result<Option<u32>, HleError> {
    if ctx.memory.read_u32(address + queue::STATE)? != queue_state::RUN {
        return Ok(None);
    }
    let next = ctx.memory.read_u32(address + queue::NEXT + SLOT)?;
    if next == ctx.memory.read_u32(address + queue::END + SLOT)? {
        return Ok(None);
    }
    ctx.memory.write_u32(address + queue::NEXT + SLOT, next + 1)?;
    ctx.memory.write_u32(
        address + queue::READY,
        ctx.memory.read_u32(address + queue::READY)?.wrapping_sub(1),
    )?;
    ctx.memory
        .write_u32(address + queue::RUNNING, ctx.memory.read_u32(address + queue::RUNNING)? + 1)?;
    let tasks = ctx.memory.read_u32(address + queue::TASKS)?;
    Ok(Some(ctx.memory.read_u32(tasks + 4 * next)?))
}

/// Runs tasks until the queue is empty; returns whether any ran or it is done.
#[hle_export(lib = "coreinit", name = "MPRunTasksFromTaskQ")]
fn mp_run_tasks_from_task_q(
    ctx: &mut HleContext<'_>,
    address: u32,
    _granularity: u32,
) -> Result<(), HleError> {
    let Some(entry) = next_task(ctx, address)? else {
        ctx.cpu.gpr[3] =
            u32::from(ctx.memory.read_u32(address + queue::STATE)? & queue_state::DONE != 0);
        return Ok(());
    };
    let stub = return_stub(ctx)?;
    let me = ctx.os.coreinit.threads.current;
    let caller_lr = ctx.cpu.lr;
    ctx.os
        .coreinit
        .mp
        .running
        .insert(me, RunningTasks { queue: Some(address), task: entry, caller_lr, ran_any: true });
    enter_task(ctx, entry, stub)
}

/// Runs one task taken with `MPDequeTasks`.
#[hle_export(lib = "coreinit", name = "MPRunTask")]
fn mp_run_task(ctx: &mut HleContext<'_>, entry: u32) -> Result<(), HleError> {
    if ctx.memory.read_u32(entry + task::STATE)? != task_state::READY {
        ctx.cpu.gpr[3] = 0;
        return Ok(());
    }
    let address = ctx.memory.read_u32(entry + task::QUEUE)?;
    let state = ctx.memory.read_u32(address + queue::STATE)?;
    if state != queue_state::RUN {
        ctx.cpu.gpr[3] = 0;
        return Ok(());
    }
    ctx.memory.write_u32(
        address + queue::READY,
        ctx.memory.read_u32(address + queue::READY)?.wrapping_sub(1),
    )?;
    ctx.memory
        .write_u32(address + queue::RUNNING, ctx.memory.read_u32(address + queue::RUNNING)? + 1)?;
    let stub = return_stub(ctx)?;
    let me = ctx.os.coreinit.threads.current;
    let caller_lr = ctx.cpu.lr;
    ctx.os
        .coreinit
        .mp
        .running
        .insert(me, RunningTasks { queue: None, task: entry, caller_lr, ran_any: true });
    enter_task(ctx, entry, stub)
}

/// A task function returned.
#[hle_export(lib = "shocora", name = "__shocora_MPTaskReturn")]
fn mp_task_return(ctx: &mut HleContext<'_>) -> Result<(), HleError> {
    let me = ctx.os.coreinit.threads.current;
    let Some(run) = ctx.os.coreinit.mp.running.remove(&me) else {
        return Err(HleError::Fatal("MP task returned without a running task".into()));
    };
    let result = ctx.cpu.gpr[3];
    ctx.memory.write_u32(run.task + task::RESULT, result)?;
    ctx.memory.write_u32(run.task + task::STATE, task_state::DONE)?;
    let address = ctx.memory.read_u32(run.task + task::QUEUE)?;
    ctx.memory.write_u32(
        address + queue::RUNNING,
        ctx.memory.read_u32(address + queue::RUNNING)?.wrapping_sub(1),
    )?;
    let done = ctx.memory.read_u32(address + queue::DONE + SLOT)? + 1;
    ctx.memory.write_u32(address + queue::DONE + SLOT, done)?;
    let state = ctx.memory.read_u32(address + queue::STATE)?;
    if state == queue_state::STOPPING && ctx.memory.read_u32(address + queue::RUNNING)? == 0 {
        ctx.memory.write_u32(address + queue::STATE, queue_state::STOP)?;
    }
    if ctx.memory.read_u32(address + queue::TASK_COUNT)? == done {
        ctx.memory.write_u32(address + queue::STATE, queue_state::DONE)?;
    }

    if let Some(queue) = run.queue {
        if let Some(entry) = next_task(ctx, queue)? {
            let stub = return_stub(ctx)?;
            ctx.os.coreinit.mp.running.insert(me, RunningTasks { task: entry, ..run });
            return enter_task(ctx, entry, stub);
        }
    }
    // Back to the original caller: this stub's `blr` follows `lr`.
    ctx.cpu.lr = run.caller_lr;
    ctx.cpu.gpr[3] = u32::from(run.ran_any);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hle::test_env::{SCRATCH, TestEnv};

    #[test]
    fn enque_and_deque_keep_queue_counters() {
        let mut env = TestEnv::new();
        let (q, tasks, t0, t1, out) =
            (SCRATCH, SCRATCH + 0x100, SCRATCH + 0x200, SCRATCH + 0x300, SCRATCH + 0x400);
        env.call("coreinit", "MPInitTaskQ", &[q, tasks, 4]);
        env.call("coreinit", "MPInitTask", &[t0, 0x1000, 7, 8]);
        env.call("coreinit", "MPInitTask", &[t1, 0x2000, 9, 10]);
        assert_eq!(env.call("coreinit", "MPEnqueTask", &[q, t0]), 1);
        assert_eq!(env.call("coreinit", "MPEnqueTask", &[q, t1]), 1);
        assert_eq!(env.memory.read_u32(q + queue::READY).unwrap(), 2);
        assert_eq!(env.call("coreinit", "MPDequeTasks", &[q, out, 1]), 0, "queue not started");
        assert_eq!(env.call("coreinit", "MPStartTaskQ", &[q]), 1);
        assert_eq!(env.call("coreinit", "MPDequeTasks", &[q, out, 1]), 1);
        assert_eq!(env.memory.read_u32(out).unwrap(), t0);
    }
}
