//! Calling guest functions from the OS (alarm handlers, FS completion callbacks).
//!
//! A call is injected on the main core at a safe point (right after an HLE
//! call or between instructions): the interrupted registers are saved, the
//! callback runs on a dedicated stack and returns into an internal stub that
//! restores them. Calls run one at a time, in order.
use std::collections::VecDeque;

use wiiu_cpu::CpuState;

/// A guest function to call with up to eight integer arguments.
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct GuestCall {
    pub function: u32,
    pub args: Vec<u32>,
}

/// Pending and running callbacks.
#[derive(serde::Serialize, serde::Deserialize, Debug, Default)]
pub struct Callbacks {
    queue: VecDeque<GuestCall>,
    /// Registers of the interrupted code while a callback runs.
    saved: Option<CpuState>,
    /// Top of the callback stack (set at boot).
    pub stack_top: u32,
    /// Incremented on every start/finish, so callers can detect a switch.
    pub generation: u64,
}

impl Callbacks {
    #[inline]
    pub fn push(&mut self, call: GuestCall) {
        self.queue.push_back(call);
    }

    #[inline]
    pub const fn active(&self) -> bool {
        self.saved.is_some()
    }

    #[inline]
    pub fn has_pending(&self) -> bool {
        !self.queue.is_empty()
    }

    /// Calls waiting to run.
    #[inline]
    pub fn pending(&self) -> usize {
        self.queue.len()
    }

    /// Starts the next queued call on `cpu`; `return_stub` becomes its `lr`.
    pub fn begin(&mut self, cpu: &mut CpuState, return_stub: u32) -> bool {
        if self.saved.is_some() || self.stack_top == 0 {
            return false;
        }

        let Some(call) = self.queue.pop_front() else { return false };
        let saved = cpu.clone();

        cpu.pc = call.function;
        cpu.lr = return_stub;
        cpu.gpr[1] = self.stack_top - 0x20;

        for (i, arg) in call.args.iter().take(8).enumerate() {
            cpu.gpr[3 + i] = *arg;
        }

        self.saved = Some(saved);
        self.generation += 1;

        true
    }

    /// Restores the interrupted registers.
    pub fn finish(&mut self, cpu: &mut CpuState) {
        if let Some(saved) = self.saved.take() {
            *cpu = saved;
            self.generation += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn saves_and_restores_registers() {
        let mut callbacks = Callbacks { stack_top: 0x8000, ..Callbacks::default() };
        let mut cpu = CpuState { pc: 0x1234, ..CpuState::default() };

        cpu.gpr[3] = 7;
        callbacks.push(GuestCall { function: 0x4000, args: vec![1, 2] });

        assert!(callbacks.begin(&mut cpu, 0x5000));
        assert_eq!((cpu.pc, cpu.lr, cpu.gpr[3], cpu.gpr[4]), (0x4000, 0x5000, 1, 2));
        assert!(!callbacks.begin(&mut cpu, 0x5000), "one callback at a time");

        callbacks.finish(&mut cpu);

        assert_eq!((cpu.pc, cpu.gpr[3]), (0x1234, 7));
        assert!(!callbacks.active());
    }
}
