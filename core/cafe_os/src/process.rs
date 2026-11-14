//! Process creation: what the kernel and loader do before `main`.
//!
//! ```text
//!  MEM2: [ .data/.bss of the RPX | default (expanded) heap ............. ]
//!                                   └ main thread stack is its first block
//!  MEM1: [ frame heap (base heap 0) ]   FOREGROUND: [ frame heap (base heap 2) | OS ]
//!  HLE:  [ call stubs | OSThread of main thread, data imports ...        ]
//! ```
use wiiu_hle_macros::hle_export;
use wiiu_mem_addr::{FOREGROUND, FOREGROUND_FREE_SIZE, LOADER, MEM1, MEM2, TEXT};
use wiiu_memory::{Memory, MemoryError};
use wiiu_rpl::{LinearAllocator, LoadError, LoadedModule, RplFile, SegmentKind};

use crate::{
    CafeOs,
    coreinit::{
        mem::arena,
        thread::{
            MAIN_THREAD_PRIORITY, OS_THREAD_SIZE, ThreadRecord, ThreadState,
            offset as thread_offset, sync_guest,
        },
    },
    hle::{HleContext, HleExport},
};

/// Minimum main-thread stack.
const MIN_STACK_SIZE: u32 = 0x4_0000;
/// The default heap starts on this boundary after the module's data.
const HEAP_ALIGN: u32 = 0x1_0000;

/// Library of emulator-internal entry points.
pub const INTERNAL_LIBRARY: &str = "shocora";

#[derive(Debug, thiserror::Error)]
pub enum BootError {
    #[error(transparent)]
    Load(#[from] LoadError),
    #[error(transparent)]
    Memory(#[from] MemoryError),
    #[error("not enough MEM2 for a {0:#x}-byte main stack")]
    NoStack(u32),
}

/// Initial register state for the main thread.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BootInfo {
    pub module: LoadedModule,
    pub entry: u32,
    /// Initial `r1`.
    pub stack_pointer: u32,
    /// Initial `r2`.
    pub sda2_base: u32,
    /// Initial `r13`.
    pub sda_base: u32,
    /// Initial `lr`: returning from the entry point ends the process.
    pub return_address: u32,
    /// Initial `r3`, `r4`, `r5`.
    pub args: [u32; 3],
}

/// What runs after `__preinit_user` returns.
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub struct MainEntry {
    pub entry: u32,
    pub argc: u32,
    pub argv: u32,
    pub stack_pointer: u32,
    /// Three words: the MEM1, foreground and MEM2 heaps handed to `__preinit_user`.
    pub heaps: u32,
}

/// The main thread returned from its entry point.
#[hle_export(lib = "shocora", name = "__shocora_MainThreadReturn")]
fn main_thread_return(ctx: &mut HleContext<'_>) {
    let code = ctx.cpu.gpr[3].cast_signed();
    log::info!("main thread returned {code}");
    ctx.os.exit_code = Some(code);
    ctx.os.frame_done = true;
}

/// A guest callback returned: resume the interrupted code.
#[hle_export(lib = "shocora", name = "__shocora_CallbackReturn")]
fn callback_return(ctx: &mut HleContext<'_>) {
    ctx.os.callbacks.finish(ctx.cpu);
    if ctx.os.current_thread_blocked() {
        ctx.os.coreinit.threads.request(crate::coreinit::thread::Reschedule::Block);
    }
}

/// `__preinit_user` returned: install the heaps it chose and enter the title.
#[hle_export(lib = "shocora", name = "__shocora_PreinitReturn")]
fn preinit_return(ctx: &mut HleContext<'_>) -> Result<(), crate::hle::HleError> {
    let Some(main) = ctx.os.pending_main.take() else {
        return Err(crate::hle::HleError::Fatal("__preinit_user returned twice".into()));
    };
    ctx.os.install_base_heaps(ctx.memory, main.heaps)?;
    let return_address = ctx.os.registry.function_stub(INTERNAL_LIBRARY, MAIN_THREAD_RETURN.name);
    ctx.os.registry.commit(ctx.memory)?;
    ctx.cpu.pc = main.entry;
    ctx.cpu.lr = return_address;
    ctx.cpu.gpr[1] = main.stack_pointer;
    ctx.cpu.gpr[3] = main.argc;
    ctx.cpu.gpr[4] = main.argv;
    Ok(())
}

pub const EXPORTS: &[HleExport] = &[MAIN_THREAD_RETURN, CALLBACK_RETURN, PREINIT_RETURN];

/// Stack used by guest callbacks.
const CALLBACK_STACK_SIZE: u32 = 0x1_0000;

impl CafeOs {
    /// Loads the main executable and prepares the process.
    ///
    /// # Errors
    /// Fails if the RPX is malformed or memory runs out.
    pub fn boot(
        &mut self,
        memory: &mut Memory,
        name: &str,
        rpx: &[u8],
    ) -> Result<BootInfo, BootError> {
        let file = RplFile::parse(rpx)?;
        let mut allocator = LinearAllocator::new(TEXT, MEM2, LOADER);
        let module = wiiu_rpl::load(&file, name, memory, &mut allocator, &mut self.registry)?;
        log::info!("loaded {name}: entry {:#010x}, segments {:x?}", module.entry, module.segments);
        if let Some(&text) = module.segments.get(&SegmentKind::Text) {
            crate::patches::apply(memory, &self.registry, text)?;
        }
        self.coreinit.tls_image = module.tls;

        let heap_start = allocator.cursor(SegmentKind::Data).next_multiple_of(HEAP_ALIGN);
        let heap_size = u32::try_from(MEM2.end() - u64::from(heap_start)).unwrap_or(0);
        let stack_size = module.stack_size.max(MIN_STACK_SIZE);
        let (stack, heaps) = self.create_base_heaps(memory, heap_start, heap_size, stack_size)?;

        self.create_threads(memory, stack + stack_size, stack_size)?;
        let (argc, argv) = self.write_arguments(memory, name)?;

        // Leave a back-chain slot at the top of the stack.
        let stack_pointer = (stack + stack_size - 0x10) & !0xF;
        memory.write_u32(stack_pointer, 0)?;

        // Like coreinit, run the title's `__preinit_user(&mem1, &fg, &mem2)`
        // first so it can replace the default heaps.
        let (entry, return_address, args) = match module.exports.get("__preinit_user") {
            Some(&preinit) => {
                log::info!("calling __preinit_user at {preinit:#010x}");
                self.pending_main =
                    Some(MainEntry { entry: module.entry, argc, argv, stack_pointer, heaps });
                let stub = self.registry.function_stub(INTERNAL_LIBRARY, PREINIT_RETURN.name);
                (preinit, stub, [heaps, heaps + 4, heaps + 8])
            }
            None => {
                self.install_base_heaps(memory, heaps)?;
                let stub = self.registry.function_stub(INTERNAL_LIBRARY, MAIN_THREAD_RETURN.name);
                (module.entry, stub, [argc, argv, 0])
            }
        };
        self.registry.commit(memory)?;
        Ok(BootInfo {
            entry,
            stack_pointer,
            sda2_base: module.sda2_base,
            sda_base: module.sda_base,
            return_address,
            args,
            module,
        })
    }
}

impl CafeOs {
    /// Creates the MEM1, foreground and MEM2 default heaps and allocates the
    /// main stack from MEM2. Returns the stack and the address of a three-word
    /// block holding the heap handles (the `__preinit_user` arguments).
    fn create_base_heaps(
        &mut self,
        memory: &mut Memory,
        mem2_start: u32,
        mem2_size: u32,
        stack_size: u32,
    ) -> Result<(u32, u32), BootError> {
        use crate::coreinit::mem::{EXP_HEAP_HEADER, FRM_HEAP_HEADER, magic, write_header};
        let heaps = &mut self.coreinit.heaps;
        let mem2 =
            heaps.create_expanded(mem2_start, mem2_size).ok_or(BootError::NoStack(stack_size))?;
        write_header(memory, mem2, magic::EXPANDED, EXP_HEAP_HEADER, (mem2 + mem2_size) & !3)?;
        // The default heap must work before `__preinit_user` runs.
        heaps.set_base(arena::MEM2, mem2);
        let mem1 = heaps.create_frame(MEM1.start, MEM1.size).unwrap_or(0);
        if mem1 != 0 {
            write_header(memory, mem1, magic::FRAME, FRM_HEAP_HEADER, MEM1.start + MEM1.size)?;
        }
        let foreground = heaps.create_frame(FOREGROUND.start, FOREGROUND_FREE_SIZE).unwrap_or(0);
        if foreground != 0 {
            write_header(
                memory,
                foreground,
                magic::FRAME,
                FRM_HEAP_HEADER,
                FOREGROUND.start + FOREGROUND_FREE_SIZE,
            )?;
        }
        let stack = heaps
            .default_heap_mut()
            .and_then(|heap| heap.alloc(stack_size, 0x40))
            .ok_or(BootError::NoStack(stack_size))?;

        let block = self.registry.allocate_data(12, 4);
        for (i, handle) in [mem1, foreground, mem2].into_iter().enumerate() {
            memory.write_u32(block + 4 * i as u32, handle)?;
        }
        Ok((stack, block))
    }

    /// Installs the heaps in `block` (MEM1, foreground, MEM2) as base heaps 0, 8 and 1.
    ///
    /// # Errors
    /// Fails if `block` is unmapped.
    pub fn install_base_heaps(
        &mut self,
        memory: &Memory,
        block: u32,
    ) -> Result<(), wiiu_memory::MemoryError> {
        let heaps = &mut self.coreinit.heaps;
        for (i, arena_index) in
            [arena::MEM1, arena::FOREGROUND, arena::MEM2].into_iter().enumerate()
        {
            let handle = memory.read_u32(block + 4 * i as u32)?;
            if handle != 0 {
                heaps.replace_base(arena_index, handle);
            }
        }
        Ok(())
    }

    /// `argc`/`argv` for the entry point: `argv[0]` is the executable name.
    fn write_arguments(
        &mut self,
        memory: &mut Memory,
        name: &str,
    ) -> Result<(u32, u32), BootError> {
        let path = format!("/vol/code/{name}");
        #[allow(clippy::cast_possible_truncation)]
        let text = self.registry.allocate_data(path.len() as u32 + 1, 4);
        memory.write_c_string(text, &path)?;
        let argv = self.registry.allocate_data(8, 4);
        memory.write_u32(argv, text)?;
        memory.write_u32(argv + 4, 0)?;
        Ok((1, argv))
    }
}

/// Stack of each idle default thread (cores 0 and 2).
const DEFAULT_THREAD_STACK: u32 = 0x1_0000;

impl CafeOs {
    /// Registers the main thread (running on core 1) and the default threads
    /// of cores 0 and 2, which stay terminated until `OSRunThread` starts them.
    fn create_threads(
        &mut self,
        memory: &mut Memory,
        main_stack_top: u32,
        main_stack_size: u32,
    ) -> Result<(), BootError> {
        let main = self.registry.allocate_data(OS_THREAD_SIZE, 8);
        memory.write_u32(main + thread_offset::STACK_BASE, main_stack_top)?;
        memory.write_u32(main + thread_offset::STACK_END, main_stack_top - main_stack_size)?;
        self.coreinit.main_thread = main;
        self.coreinit.threads.adopt_running(main, MAIN_THREAD_PRIORITY);
        if let Some(record) = self.coreinit.threads.threads.get(&main) {
            sync_guest(memory, main, record)?;
        }

        let mut defaults = [0; 3];
        for core in [0, 2] {
            let thread = self.registry.allocate_data(OS_THREAD_SIZE, 8);
            let stack = self
                .coreinit
                .heaps
                .default_heap_mut()
                .and_then(|heap| heap.alloc(DEFAULT_THREAD_STACK, 0x40))
                .ok_or(BootError::NoStack(DEFAULT_THREAD_STACK))?;
            memory.write_u32(thread + thread_offset::STACK_BASE, stack + DEFAULT_THREAD_STACK)?;
            memory.write_u32(thread + thread_offset::STACK_END, stack)?;
            let mut record = ThreadRecord::new(wiiu_cpu::CpuState::new(core), MAIN_THREAD_PRIORITY);
            record.state = ThreadState::Moribund;
            record.suspend_count = 0;
            sync_guest(memory, thread, &record)?;
            self.coreinit.threads.threads.insert(thread, record);
            defaults[core as usize] = thread;
        }
        let callback_stack = self
            .coreinit
            .heaps
            .default_heap_mut()
            .and_then(|heap| heap.alloc(CALLBACK_STACK_SIZE, 0x40))
            .ok_or(BootError::NoStack(CALLBACK_STACK_SIZE))?;
        self.callbacks.stack_top = callback_stack + CALLBACK_STACK_SIZE;
        defaults[1] = main;
        self.coreinit.default_threads = defaults;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use crate::hle::test_env::TestEnv;

    /// Intent: when the title's `main` returns, its return value becomes the exit status
    /// and the frame ends.
    /// Pass: r3 at the return stub is the exit code.
    #[test]
    fn main_returning_exits_with_its_value() {
        let mut env = TestEnv::new();
        env.call("shocora", "__shocora_MainThreadReturn", &[5]);
        assert_eq!(env.os.exit_code, Some(5));
        assert!(env.os.frame_done);
    }
}
