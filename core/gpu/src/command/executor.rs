//! Executes PM4 packets against the register file, guest memory and display.
use std::collections::HashMap;

use wiiu_interrupt::{Interrupt, PendingInterrupts};
use wiiu_memory::Memory;

use super::{
    opcode,
    pm4::{Packet, parse},
};
use crate::{
    GpuError, Screen,
    display::Display,
    latte::{DrawRequest, Latte, SurfaceWords, index_type},
    regs::{RegisterFile, base, context as reg},
};

/// Maximum nesting of `INDIRECT_BUFFER`.
const MAX_INDIRECT_DEPTH: u32 = 4;

/// What the command processor needs besides its own state.
pub struct CpContext<'a> {
    pub memory: &'a mut Memory,
    pub display: &'a mut Display,
    pub interrupts: &'a mut PendingInterrupts,
    pub latte: &'a mut Latte,
    /// GPU timestamp written by `EVENT_WRITE_EOP`.
    pub timestamp: u64,
}

/// Counters for the debugger / status bar.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CpStats {
    pub packets: u64,
    pub draws: u64,
    pub unknown_opcodes: u64,
}

/// State a GX2 context restores when it becomes current again.
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
struct SavedContext {
    regs: RegisterFile,
    color_targets: [SurfaceWords; 8],
    depth_target: SurfaceWords,
}

/// The command processor.
#[derive(serde::Serialize, serde::Deserialize, Debug, Default, Clone)]
pub struct CommandProcessor {
    pub regs: RegisterFile,
    #[serde(skip)]
    pub stats: CpStats,
    index_type: u32,
    instances: u32,
    contexts: HashMap<u32, SavedContext>,
    current_context: u32,
}

fn translate(physical: u32) -> Result<u32, GpuError> {
    wiiu_mem_addr::physical_to_effective(physical).ok_or(GpuError::BadPhysicalAddress(physical))
}

impl CommandProcessor {
    /// Executes a command buffer.
    ///
    /// # Errors
    /// Fails on malformed packets or accesses outside guest memory.
    #[inline]
    pub fn execute(&mut self, words: &[u32], ctx: &mut CpContext<'_>) -> Result<(), GpuError> {
        self.execute_nested(words, ctx, 0)
    }

    fn execute_nested(
        &mut self,
        words: &[u32],
        ctx: &mut CpContext<'_>,
        depth: u32,
    ) -> Result<(), GpuError> {
        for packet in parse(words) {
            self.stats.packets += 1;
            match packet? {
                Packet::Type0 { register, values } => self.regs.set_range(register, values),
                Packet::Type2 => {}
                Packet::Type3 { opcode, body } => self.type3(opcode, body, ctx, depth)?,
            }
        }
        Ok(())
    }

    fn set_registers(&mut self, space: u32, body: &[u32]) {
        if let [offset, values @ ..] = body {
            self.regs.set_range(space + offset, values);
        }
    }

    fn save_context(&mut self, ctx: &CpContext<'_>) {
        if self.current_context != 0 {
            let saved = SavedContext {
                regs: self.regs.clone(),
                color_targets: ctx.latte.color_targets,
                depth_target: ctx.latte.depth_target,
            };
            self.contexts.insert(self.current_context, saved);
        }
    }

    fn context_state(&mut self, body: &[u32], ctx: &mut CpContext<'_>) {
        let [action, state, ..] = body else { return };
        if *action == 0 {
            // A new context: it starts from the state just programmed.
            self.save_context(ctx);
            self.current_context = *state;
            return;
        }
        if *state == self.current_context {
            return;
        }
        // State 0 turns shadowing off: what is set from now on belongs to no context.
        self.save_context(ctx);
        self.current_context = *state;
        if let Some(saved) = self.contexts.get(state) {
            self.regs = saved.regs.clone();
            ctx.latte.color_targets = saved.color_targets;
            ctx.latte.depth_target = saved.depth_target;
        }
    }

    fn draw(
        &mut self,
        ctx: &mut CpContext<'_>,
        count: u32,
        indices: Option<&[u8]>,
        immediate: bool,
        index_address: Option<u32>,
    ) {
        self.stats.draws += 1;
        let request = DrawRequest {
            count,
            instances: self.instances,
            base_vertex: self.regs.get(base::CTL_CONST),
            base_instance: self.regs.get(base::CTL_CONST + 1),
            index_type: self.index_type,
            indices,
            immediate,
            index_address,
        };
        ctx.latte.draw(ctx.memory, &mut self.regs, &request);
    }

    fn type3(
        &mut self,
        op: u8,
        body: &[u32],
        ctx: &mut CpContext<'_>,
        depth: u32,
    ) -> Result<(), GpuError> {
        match op {
            opcode::NOP
            | opcode::CONTEXT_CONTROL
            | opcode::SURFACE_SYNC
            | opcode::EVENT_WRITE
            | opcode::WAIT_REG_MEM => {}
            opcode::SET_CONFIG_REG => self.set_registers(base::CONFIG, body),
            opcode::SET_CONTEXT_REG => self.set_registers(base::CONTEXT, body),
            opcode::SET_ALU_CONST => self.set_registers(base::ALU_CONST, body),
            opcode::SET_BOOL_CONST => self.set_registers(base::BOOL_CONST, body),
            opcode::SET_LOOP_CONST => self.set_registers(base::LOOP_CONST, body),
            opcode::SET_RESOURCE => self.set_registers(base::RESOURCE, body),
            opcode::SET_SAMPLER => self.set_registers(base::SAMPLER, body),
            opcode::SET_CTL_CONST => self.set_registers(base::CTL_CONST, body),
            opcode::INDIRECT_BUFFER => {
                if depth >= MAX_INDIRECT_DEPTH {
                    return Err(GpuError::IndirectTooDeep(MAX_INDIRECT_DEPTH));
                }
                if let [address_lo, _address_hi, size, ..] = body {
                    let start = translate(*address_lo & !3)?;
                    let words: Vec<u32> = ctx
                        .memory
                        .slice(start, size.saturating_mul(4))?
                        .as_chunks::<4>()
                        .0
                        .iter()
                        .map(|b| u32::from_be_bytes(*b))
                        .collect();
                    self.execute_nested(&words, ctx, depth + 1)?;
                }
            }
            opcode::MEM_WRITE => {
                if let [address_lo, control, data_lo, data_hi, ..] = body {
                    let address = translate(*address_lo & !3)?;
                    ctx.memory.write_u32(address, *data_lo)?;
                    // DATA32 (bit 18) clear: also write the high dword.
                    if control & (1 << 18) == 0 {
                        ctx.memory.write_u32(address + 4, *data_hi)?;
                    }
                }
            }
            opcode::STRMOUT_BUFFER_UPDATE => {
                if let [control, write_address, _, read_address, ..] = body {
                    let buffer = (control >> 8) & 3;
                    let register = reg::VGT_STRMOUT_BUFFER_OFFSET_0 + buffer * 4;
                    match (control >> 1) & 3 {
                        0 => self.regs.set(register, 0),
                        // The offset a saved context holds.
                        2 => {
                            let saved = ctx.memory.read_u32(translate(*read_address)?)?;
                            self.regs.set(register, saved);
                        }
                        3 => {
                            let at = translate(*write_address)?;
                            ctx.memory.write_u32(at, self.regs.get(register))?;
                        }
                        mode => log::debug!("STRMOUT_BUFFER_UPDATE mode {mode}"),
                    }
                }
            }
            opcode::EVENT_WRITE_EOP => {
                if let [_event, address_lo, control, data_lo, data_hi, ..] = body {
                    let address = translate(*address_lo & !3)?;
                    match control >> 29 {
                        1 => ctx.memory.write_u32(address, *data_lo)?,
                        2 => {
                            ctx.memory.write_u32(address, *data_lo)?;
                            ctx.memory.write_u32(address + 4, *data_hi)?;
                        }
                        3 => ctx.memory.write_u64(address, ctx.timestamp)?,
                        _ => {}
                    }
                    if (control >> 24) & 3 != 0 {
                        ctx.interrupts.raise(Interrupt::GpuEndOfPipe);
                    }
                }
            }
            opcode::INDEX_TYPE => {
                self.index_type = body.first().copied().unwrap_or(index_type::U32_BE);
            }
            opcode::NUM_INSTANCES => self.instances = body.first().copied().unwrap_or(1),
            opcode::DRAW_INDEX_AUTO => {
                if let [count, ..] = body {
                    self.draw(ctx, *count, None, false, None);
                }
            }
            opcode::DRAW_INDEX_2 => {
                if let [_max, address, _high, count, ..] = body {
                    let effective = translate(*address)?;
                    let width =
                        if matches!(self.index_type, index_type::U32_LE | index_type::U32_BE) {
                            4
                        } else {
                            2
                        };
                    // Checked here; Latte reads them when its copy is out of date.
                    ctx.memory.slice(effective, count.saturating_mul(width))?;
                    self.draw(ctx, *count, None, false, Some(effective));
                }
            }
            opcode::DRAW_INDEX_IMMD => {
                if let [count, _initiator, indices @ ..] = body {
                    let bytes: Vec<u8> = indices.iter().flat_map(|w| w.to_be_bytes()).collect();
                    self.draw(ctx, *count, Some(&bytes), true, None);
                }
            }
            opcode::HLE_CLEAR => {
                if let [flags, rest @ ..] = body {
                    let color = SurfaceWords::parse(rest);
                    let depth_words =
                        rest.get(opcode::SURFACE_WORDS..).and_then(SurfaceWords::parse);
                    if let Some(values) =
                        rest.get(2 * opcode::SURFACE_WORDS..2 * opcode::SURFACE_WORDS + 6)
                    {
                        let rgba = [values[0], values[1], values[2], values[3]].map(f32::from_bits);
                        ctx.latte.clear(
                            *flags,
                            color.as_ref(),
                            depth_words.as_ref(),
                            rgba,
                            f32::from_bits(values[4]),
                            values[5],
                        );
                    }
                }
            }
            opcode::HLE_COPY_COLOR_BUFFER_TO_SCAN_BUFFER => {
                if let (Some(surface), Some(target)) =
                    (SurfaceWords::parse(body), body.get(opcode::SURFACE_WORDS))
                {
                    if target & opcode::scan_target::TV != 0 {
                        ctx.latte.copy_to_scan(&surface, Screen::Tv);
                    }
                    if target & opcode::scan_target::DRC != 0 {
                        ctx.latte.copy_to_scan(&surface, Screen::Drc);
                    }
                }
            }
            opcode::HLE_SWAP_SCAN_BUFFERS => {
                ctx.display.swap(ctx.latte);
                ctx.interrupts.raise(Interrupt::GpuFlip);
            }
            opcode::HLE_CONTEXT_STATE => self.context_state(body, ctx),
            opcode::HLE_COPY_SURFACE => {
                let source = SurfaceWords::parse(body);
                let target = body.get(opcode::SURFACE_WORDS..).and_then(SurfaceWords::parse);
                if let (Some(source), Some(target)) = (source, target) {
                    ctx.latte.copy_surface(ctx.memory, &source, &target);
                }
            }
            opcode::HLE_SET_TARGET => {
                if let [slot, rest @ ..] = body {
                    if let Some(surface) = SurfaceWords::parse(rest) {
                        match *slot {
                            0..=7 => ctx.latte.color_targets[*slot as usize] = surface,
                            _ => ctx.latte.depth_target = surface,
                        }
                    }
                }
            }
            other => {
                self.stats.unknown_opcodes += 1;
                log::trace!("unhandled PM4 opcode {other:#04x}");
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use wiiu_mem_addr::MEM2;

    use super::*;
    use crate::{RenderCommand, command::pm4::type3_header};

    fn memory() -> Memory {
        let mut memory = Memory::new();
        memory.map(wiiu_mem_addr::Region::new("MEM2", MEM2.start, 0x10_0000)).unwrap();
        memory
    }

    #[test]
    fn register_writes_and_indirect_buffers() {
        let mut memory = memory();
        let ib = MEM2.start + 0x1000;
        for (i, word) in
            [type3_header(opcode::SET_CONFIG_REG, 2), 0x10, 0xABCD].into_iter().enumerate()
        {
            memory.write_u32(ib + 4 * i as u32, word).unwrap();
        }
        let physical = wiiu_mem_addr::effective_to_physical(ib).unwrap();
        let words = [type3_header(opcode::INDIRECT_BUFFER, 3), physical, 0, 3];

        let (mut display, mut interrupts, mut latte) =
            (Display::default(), PendingInterrupts::default(), Latte::default());
        let mut ctx = CpContext {
            memory: &mut memory,
            display: &mut display,
            interrupts: &mut interrupts,
            latte: &mut latte,
            timestamp: 9,
        };
        let mut cp = CommandProcessor::default();
        cp.execute(&words, &mut ctx).unwrap();
        assert_eq!(cp.regs.get(base::CONFIG + 0x10), 0xABCD);
    }

    #[test]
    fn clear_copy_and_swap_reach_the_renderer() {
        let mut memory = memory();
        let surface = [0x5000_0000, 2, 1, 1, 2, 4, 0, 0x1A, 0, 0];
        let mut words = vec![
            type3_header(opcode::HLE_CLEAR, 1 + 2 * opcode::SURFACE_WORDS + 6),
            opcode::clear::COLOR,
        ];
        words.extend(surface);
        words.extend([0; opcode::SURFACE_WORDS]);
        words.extend([0.0_f32, 1.0, 0.0, 1.0].map(f32::to_bits));
        words.extend([0, 0]);
        words.push(type3_header(
            opcode::HLE_COPY_COLOR_BUFFER_TO_SCAN_BUFFER,
            1 + opcode::SURFACE_WORDS,
        ));
        words.extend(surface);
        words.push(opcode::scan_target::DRC);
        words.extend([type3_header(opcode::HLE_SWAP_SCAN_BUFFERS, 1), 0]);

        let (mut display, mut interrupts, mut latte) =
            (Display::default(), PendingInterrupts::default(), Latte::default());
        let mut ctx = CpContext {
            memory: &mut memory,
            display: &mut display,
            interrupts: &mut interrupts,
            latte: &mut latte,
            timestamp: 0,
        };
        CommandProcessor::default().execute(&words, &mut ctx).unwrap();

        assert!(matches!(latte.commands[0], RenderCommand::Clear { color: Some(_), depth: None }));
        assert!(matches!(
            latte.commands[1],
            RenderCommand::CopyToScanBuffer { screen: Screen::Drc, .. }
        ));
        assert!(matches!(latte.commands[2], RenderCommand::Flip));
        assert!(interrupts.is_pending(Interrupt::GpuFlip));
    }
}
