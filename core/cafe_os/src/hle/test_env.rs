//! A minimal machine for HLE unit tests.
use wiiu_cpu::CpuState;
use wiiu_gpu::Gpu;
use wiiu_iosu::Iosu;
use wiiu_mem_addr::{HLE, MEM2, Region};
use wiiu_memory::Memory;

use super::HleContext;
use crate::{CafeOs, coreinit::mem::arena};

/// Scratch MEM2 base for tests.
pub const SCRATCH: u32 = MEM2.start;
/// Heap used by tests.
pub const HEAP_START: u32 = MEM2.start + 0x10_0000;
/// `OSThread` of the test main thread.
pub const MAIN_THREAD: u32 = MEM2.start + 0x3_0000;

pub struct TestEnv {
    pub cpu: CpuState,
    pub memory: Memory,
    pub os: CafeOs,
    pub gpu: Gpu,
    pub iosu: Iosu,
}

impl TestEnv {
    pub fn new() -> Self {
        let mut memory = Memory::new();
        memory.map(Region::new("MEM2", MEM2.start, 0x40_0000)).unwrap();
        memory.map(HLE).unwrap();
        memory.map(wiiu_mem_addr::TILING_APERTURE).unwrap();
        let mut cpu = CpuState::new(1);
        cpu.gpr[1] = MEM2.start + 0x8_0000;
        let mut os = CafeOs::new();
        let heap = os.coreinit.heaps.create_expanded(HEAP_START, 0x10_0000).unwrap();
        os.coreinit.heaps.set_base(arena::MEM2, heap);
        os.coreinit.main_thread = MAIN_THREAD;
        os.coreinit.threads.adopt_running(MAIN_THREAD, 16);
        Self { cpu, memory, os, gpu: Gpu::new(), iosu: Iosu::default() }
    }

    pub fn ctx(&mut self) -> HleContext<'_> {
        HleContext {
            cpu: &mut self.cpu,
            memory: &mut self.memory,
            os: &mut self.os,
            gpu: &mut self.gpu,
            iosu: &mut self.iosu,
        }
    }

    /// Calls export `library::name` with integer arguments in r3.. and returns r3.
    pub fn call(&mut self, library: &str, name: &str, args: &[u32]) -> u32 {
        let index = self.os.registry.function_index(library, name).expect("export registered");
        // r3..=r10, then the stack parameter area at 8(r1).
        for (i, value) in args.iter().enumerate() {
            if i < 8 {
                self.cpu.gpr[3 + i] = *value;
            } else {
                #[allow(clippy::cast_possible_truncation)]
                let at = self.cpu.gpr[1] + 8 + 4 * (i as u32 - 8);
                self.memory.write_u32(at, *value).unwrap();
            }
        }
        let mut ctx = self.ctx();
        super::dispatch(&mut ctx, index).unwrap();
        self.cpu.gpr[3]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Intent: the test machine itself works: scratch memory is mapped, the default heap
    /// allocates inside its range, and the main thread is the running one.
    /// Pass: a heap allocation lands in `HEAP_START..HEAP_START + 1 MiB`, scratch reads back,
    /// and the current thread is `MAIN_THREAD`.
    #[test]
    fn the_test_machine_is_usable() {
        let mut env = TestEnv::new();
        env.memory.write_u32(SCRATCH, 7).unwrap();
        assert_eq!(env.memory.read_u32(SCRATCH).unwrap(), 7);
        let block = env.call("coreinit", "__shocora_DefaultHeapAlloc", &[0x40]);
        assert!((HEAP_START..HEAP_START + 0x10_0000).contains(&block), "{block:#x}");
        assert_eq!(env.os.coreinit.threads.current, MAIN_THREAD);
    }
}
