//! Part of [`super`]: see its documentation.
//!
//! ## Where this file sits
//!
//! ```text
//! Game files ─► Loader ─► CPU ◄─► Memory ◄─► OS (HLE) ─► GPU ─► Screen
//!                         ▲▲▲  here
//! Controllers, clock ─► OS (HLE) ─► Sound ─► Speakers
//!
//! ┌────────────────────┐   ┌──────────────────────────┐   ┌────────────────────┐
//! │ decoded            │   │ jit/emit/blocks.rs       │   │ code to compile    │
//! │ instructions       │──►│ ──────────────────────── │──►│                    │
//! │                    │   │ PowerPC → Cranelift IR:  │   │                    │
//! │                    │   │ blocks                   │   │                    │
//! └────────────────────┘   └──────────────────────────┘   └────────────────────┘
//! ```
//!
//! The "blocks" share of emit.rs (PowerPC → Cranelift IR), moved here so that no file is longer
//! than 500 lines. It works exactly as if it were still written in that file. Turns each
//! PowerPC instruction into the intermediate language of Cranelift, the compiler that produces
//! native code. Terms such as HLE, JIT or shader are explained in `docs/guide/wiiu_term.md`.

use super::*;

/// Instructions after which the rest of the region must not run natively:
/// the interpreter may have redirected `pc`.
pub(super) const fn ends_block_after_fallback(op: Opcode) -> bool {
    matches!(
        op,
        Opcode::HleCall | Opcode::Sc | Opcode::Rfi | Opcode::Invalid | Opcode::Tw | Opcode::Twi
    )
}

impl Job {
    /// The result of a translation that went wrong.
    pub(crate) fn failed(&self) -> Translated {
        Translated {
            pc: self.pc,
            entries: self.entries.clone(),
            range: self.range,
            code: None,
            alignment: 1,
            generation: self.generation,
            nanos: 0,
            optimized: self.optimized,
        }
    }

    /// The same region again, to be made as fast as it can run.
    pub(crate) fn into_optimized(self) -> Self {
        Self { optimized: true, counter: 0, ..self }
    }

    /// Has the quick translation count its runs at `counter`, a `u32` that outlives the code.
    pub(crate) fn count_runs_at(&mut self, counter: *mut u32) {
        self.counter = counter as usize;
    }

    /// The lowest address and the one after the highest instruction.
    pub(crate) const fn range(&self) -> (u32, u32) {
        self.range
    }

    /// Whether the region has code in `[start, end)`.
    pub(crate) const fn overlaps(&self, start: u32, end: u32) -> bool {
        self.range.0 < end && start < self.range.1
    }

    /// A translation of it is good for the code as of `generation`.
    pub(crate) const fn set_generation(&mut self, generation: u64) {
        self.generation = generation;
    }

    /// Leaders not to seal on reaching them: ways in from later code (loops, the entry).
    pub(super) fn late_sealed(&self, leaders: &HashMap<u32, usize>) -> Vec<bool> {
        let mut late = vec![false; leaders.len()];
        for entry in &self.entries {
            late[leaders[entry]] = true;
        }
        for &(at, op, i) in &self.instructions {
            let target = match op {
                Opcode::B => {
                    if i.aa() {
                        i.li() as u32
                    } else {
                        at.wrapping_add(i.li() as u32)
                    }
                }
                Opcode::Bc => {
                    if i.aa() {
                        i.bd() as u32
                    } else {
                        at.wrapping_add(i.bd() as u32)
                    }
                }
                _ => continue,
            };
            if target <= at
                && !i.lk()
                && let Some(&leader) = leaders.get(&target)
            {
                late[leader] = true;
            }
        }
        late
    }

    /// Instructions of the block starting at `index`.
    pub(super) fn block_length(&self, index: usize, leaders: &HashMap<u32, usize>) -> i64 {
        let mut end = index + 1;
        while let Some(&(address, ..)) = self.instructions.get(end)
            && address == self.instructions[end - 1].0.wrapping_add(4)
            && !leaders.contains_key(&address)
        {
            end += 1;
        }
        (end - index) as i64
    }
}

/// Whether nothing after `op` can run: it always branches away.
pub(super) const fn ends_block(op: Opcode, i: Instr) -> bool {
    let bo = i.rd();
    match op {
        Opcode::B => true,
        Opcode::Bc | Opcode::Bclr => bo & 0b1_0100 == 0b1_0100,
        Opcode::Bcctr => bo & 0b1_0000 != 0,
        _ => ends_block_after_fallback(op),
    }
}

/// Reads the instructions of the region entered at `pc`.
#[allow(clippy::fn_to_numeric_cast_any, reason = "native code calls it by its address")]
pub(crate) fn decode_block<B: Bus>(
    pc: u32,
    bus: &mut B,
    generation: u64,
    reach: Reach,
) -> Result<Job, CpuError> {
    let limit = match reach {
        Reach::Block => MAX_BLOCK_INSTRUCTIONS,
        Reach::Region => MAX_REGION_INSTRUCTIONS,
    };
    // Decode first so an unreadable first instruction reports a clean error.
    let first = bus.fetch(pc).map_err(|_| CpuError::Bus { pc, address: pc })?;
    let near = |address: u32| {
        address.wrapping_sub(pc.wrapping_sub(REGION_REACH)) < 2 * REGION_REACH && address & 3 == 0
    };
    let mut found: BTreeMap<u32, (Opcode, Instr)> = BTreeMap::new();
    let mut leaders = BTreeSet::new();
    let mut entries = vec![pc];
    let mut work = VecDeque::from([pc]);
    while let Some(start) = work.pop_front() {
        let mut at = start;
        let mut leader = true;
        loop {
            if found.contains_key(&at) {
                leaders.insert(at);
                break;
            }
            if found.len() >= limit {
                break;
            }
            let raw = if at == pc {
                first
            } else {
                match bus.fetch(at) {
                    Ok(raw) => raw,
                    Err(_) => break,
                }
            };
            let (op, i) = (decode(raw), Instr(raw));
            found.insert(at, (op, i));
            if leader {
                leaders.insert(at);
            }
            let next = at.wrapping_add(4);
            let branch = matches!(op, Opcode::B | Opcode::Bc | Opcode::Bclr | Opcode::Bcctr);
            let target = match op {
                Opcode::B => {
                    Some(if i.aa() { i.li() as u32 } else { at.wrapping_add(i.li() as u32) })
                }
                Opcode::Bc => {
                    Some(if i.aa() { i.bd() as u32 } else { at.wrapping_add(i.bd() as u32) })
                }
                _ => None,
            };
            if branch && i.lk() {
                // The call leaves the region; where it returns to is another way in.
                if reach == Reach::Region && near(next) {
                    work.push_back(next);
                    entries.push(next);
                }
                break;
            }
            if let Some(target) = target
                && reach == Reach::Region
                && near(target)
            {
                work.push_back(target);
            }
            if ends_block(op, i) {
                break;
            }
            leader = branch;
            at = next;
        }
    }
    let mut seen = HashSet::new();
    entries.retain(|entry| found.contains_key(entry) && seen.insert(*entry));
    let range = (
        found.keys().next().copied().unwrap_or(pc),
        found.keys().next_back().map_or(pc, |last| last.wrapping_add(4)),
    );
    let helper =
        interpret_one::<B> as unsafe extern "C" fn(*mut CpuState, *mut HelperContext) -> u32;
    Ok(Job {
        pc,
        instructions: found.into_iter().map(|(at, (op, i))| (at, op, i)).collect(),
        leaders: leaders.into_iter().collect(),
        entries,
        range,
        helper: helper as usize,
        generation,
        optimized: false,
        counter: 0,
    })
}

impl Worker {
    pub(crate) fn new(isa: OwnedTargetIsa, optimizing_isa: OwnedTargetIsa, pages: usize) -> Self {
        Self {
            isa,
            optimizing_isa,
            context: Context::new(),
            builder_context: FunctionBuilderContext::new(),
            pages,
        }
    }

    pub(crate) fn translate(&mut self, job: &Job) -> Translated {
        let timer = std::time::Instant::now();
        let code = self.compile(job);
        self.context.clear();
        let (code, alignment) = code.map_or((None, 1), |(code, alignment)| (Some(code), alignment));
        Translated {
            pc: job.pc,
            entries: job.entries.clone(),
            range: job.range,
            code,
            alignment,
            generation: job.generation,
            nanos: timer.elapsed().as_nanos() as u64,
            optimized: job.optimized,
        }
    }

    pub(super) fn compile(&mut self, job: &Job) -> Option<(Vec<u8>, u64)> {
        let pointer = self.isa.pointer_type();
        let call_conv = self.isa.default_call_conv();
        self.context.func.signature = block_signature(pointer);
        {
            let mut b = FunctionBuilder::new(&mut self.context.func, &mut self.builder_context);
            let entry = b.create_block();
            b.append_block_params_for_function_params(entry);
            b.switch_to_block(entry);
            b.seal_block(entry);
            let (state, memory, helper_context) =
                (b.block_params(entry)[0], b.block_params(entry)[1], b.block_params(entry)[2]);
            // What the entry loads is only known once the region is translated.
            let prologue = b.create_block();
            b.ins().jump(prologue, &[]);

            let mut signature = Signature::new(call_conv);
            signature.params.extend([AbiParam::new(pointer); 2]);
            signature.returns.push(AbiParam::new(I32));
            let interpret_sig = b.import_signature(signature);
            let block_sig = b.import_signature(block_signature(pointer));
            let registers = std::array::from_fn(|register| {
                let ty = match register_slot(register).1 {
                    I8 => I32,
                    ty => ty,
                };
                b.declare_var(ty)
            });
            let remaining = b.declare_var(I64);
            let leader_of: HashMap<u32, usize> =
                job.leaders.iter().enumerate().map(|(index, &address)| (address, index)).collect();
            let leaders = job.leaders.iter().map(|_| b.create_block()).collect();
            let late = job.late_sealed(&leader_of);

            let mut emit = Emit {
                b: &mut b,
                state,
                memory,
                helper_context,
                interpret: job.helper as i64,
                interpret_sig,
                block_sig,
                pages: self.pages,
                counter: job.counter,
                registers,
                remaining,
                touched: 0,
                dirty: Dirty { leader: 0, local: 0, flushed: false },
                leaders,
                leader_of,
                edges: Vec::new(),
                stubs: Vec::new(),
            };
            let mut open = false;
            for (index, &(address, op, i)) in job.instructions.iter().enumerate() {
                if let Some(&leader) = emit.leader_of.get(&address) {
                    let block = emit.leaders[leader];
                    if open {
                        emit.edges.push((emit.dirty, leader));
                        emit.b.ins().jump(block, &[]);
                    }
                    emit.b.switch_to_block(block);
                    // Every way here is known once it is reached, but for loops and entries.
                    if !late[leader] {
                        emit.b.seal_block(block);
                    }
                    emit.dirty = Dirty { leader, local: 0, flushed: false };
                    emit.charge(job.block_length(index, &emit.leader_of));
                    open = true;
                } else if !open {
                    continue;
                }
                match emit.instruction(op, i, address) {
                    Flow::Next => {}
                    Flow::Ended => {
                        open = false;
                        continue;
                    }
                    Flow::Fallback if ends_block_after_fallback(op) => {
                        let stub = emit.stub(StubKind::Fallback { pc: address, then: Then::Leave });
                        emit.b.ins().jump(stub, &[]);
                        open = false;
                        continue;
                    }
                    Flow::Fallback => {
                        let resume = emit.b.create_block();
                        let then = Then::Resume(resume);
                        let stub = emit.stub(StubKind::Fallback { pc: address, then });
                        emit.b.ins().jump(stub, &[]);
                        emit.b.switch_to_block(resume);
                        emit.dirty.local = 0;
                        emit.dirty.flushed = true;
                    }
                }
                let next = address.wrapping_add(4);
                if job.instructions.get(index + 1).map(|following| following.0) != Some(next) {
                    let exit = emit.stub(StubKind::Exit(Target::Known(next)));
                    emit.b.ins().jump(exit, &[]);
                    open = false;
                }
            }
            emit.finish_stubs();

            emit.b.switch_to_block(prologue);
            emit.b.seal_block(prologue);
            // Three instructions per run tell the hot regions, the ones worth optimizing,
            // from the many that run a few times only.
            emit.count_run();
            emit.reload(emit.touched);
            let offset = offset_of!(HelperContext, remaining) as i32;
            let left = emit.b.ins().load(I64, flags(), helper_context, offset);
            emit.b.def_var(remaining, left);
            let first = emit.leaders[emit.leader_of[&job.pc]];
            if job.entries.len() == 1 {
                emit.b.ins().jump(first, &[]);
            } else {
                let pc = emit.load_state(offset_of!(CpuState, pc) as i32);
                let mut switch = Switch::new();
                for entry in &job.entries[1..] {
                    switch.set_entry(u128::from(*entry), emit.leaders[emit.leader_of[entry]]);
                }
                switch.emit(emit.b, pc, first);
            }
            b.seal_all_blocks();
            b.finalize(self.isa.frontend_config());
        }
        let isa = if job.optimized { &self.optimizing_isa } else { &self.isa };
        let compiled = match self.context.compile(&**isa, &mut ControlPlane::default()) {
            Ok(compiled) => compiled,
            Err(error) => {
                log::error!("JIT failed at {:#010x}: {:?}", job.pc, error.inner);
                return None;
            }
        };
        if !compiled.buffer.relocs().is_empty() {
            log::error!("JIT region at {:#010x} needs relocations", job.pc);
            return None;
        }
        Some((compiled.code_buffer().to_vec(), u64::from(compiled.buffer.min_alignment)))
    }
}
