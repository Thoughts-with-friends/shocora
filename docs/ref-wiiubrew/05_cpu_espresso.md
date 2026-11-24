# 5. The Espresso CPU

[Espresso](../guide/wiiu_term.md#t2-2) is three [PowerPC](../guide/wiiu_term.md#t2-7) 750-class
[cores](../guide/wiiu_term.md#t2-3), a direct descendant of the GameCube's "Gekko" and the
Wii's "Broadway". Each core reads 4-byte instructions from memory and executes them one by one,
with a handful of tricks for 3D maths. This chapter is what the emulator's CPU part must copy.

## 5.1 One core, from the outside

```text
            ┌──────────────────────────── one Espresso core ────────────────────────────┐
 memory ───►│ fetch ─► decode ─► ┌ integer units  (add, shift, compare)                │
 (code)     │  (PC)              ├ load/store unit (all memory access, byte-swapping   │
            │                    │                  happens on the host side)          │
            │                    ├ FPU + paired singles (float maths, 2 lanes)         │
            │                    └ branch unit    (uses CR, LR, CTR)                   │
            │                                                                           │
            │ registers: r0–r31, f0–f31 (×2 lanes), CR, LR, CTR, XER, FPSCR, MSR, SPRs │
            │ L1 I/D 32 KiB each · L2 · time base · decrementer                         │
            └───────────────────────────────────────────────────────────────────────────┘
```

## 5.2 The register file

```text
  general purpose (integers, pointers)      floating point (each holds two values)
  ┌─────┬─────┬─────┬─────┬ … ┬─────┐       ┌──────────────┬──────────────┐
  │ r0  │ r1  │ r2  │ r3  │   │ r31 │       │ f0.ps0 (f64) │ f0.ps1 (f32) │
  └─────┴──┬──┴──┬──┴──┬──┴ … ┴─────┘       │ …            │ …            │
           │     │     └─ 1st argument /    │ f31.ps0      │ f31.ps1      │
           │     │        return value      └──────────────┴──────────────┘
           │     └─ small-data pointer (.sdata2)
           └─ stack pointer

  special
  ┌──────┬─────────────────────────────────────────────────────────────────┐
  │ PC   │ address of the next instruction                                 │
  │ LR   │ link register: where `blr` returns to                           │
  │ CTR  │ count register: loop counter, or target of `bctr` (vtables)     │
  │ CR   │ 8 condition fields × 4 bits (lt, gt, eq, so), set by compares  │
  │ XER  │ carry / overflow bits for multi-word arithmetic                 │
  │ FPSCR│ float rounding mode, exception flags                           │
  │ MSR  │ machine state (interrupts on/off, user/supervisor)              │
  │ SPRs │ ~1000 numbered special registers: GQR0–7, time base, UPIR…      │
  └──────┴─────────────────────────────────────────────────────────────────┘
```

A [thread](../guide/wiiu_term.md#t4-10) is, from the CPU's point of view, nothing more than a
saved copy of this file. Switching threads ([context switch](../guide/wiiu_term.md#t4-16)) means
storing one copy and loading another.

**Calling convention** (what a function call looks like, important for HLE):

```text
  caller                                   callee (e.g. an HLE function in Rust)
  r3, r4, … r10 = integer arguments  ─►    reads arguments from r3…r10, f1…f8
  f1 … f8       = float arguments          further arguments on the stack at r1 + 8
  bl function   (LR = return address)      result into r3 (or r3:r4, or f1)
                                     ◄─    blr  (jump to LR)
```

## 5.3 Instructions: a fixed-width language

Every instruction is 32 bits. The top 6 bits are the primary opcode; many opcodes have an
extended opcode in the low bits.

```text
   31      26 25    21 20    16 15                        0
  ┌──────────┬────────┬────────┬───────────────────────────┐
  │ opcd = 14│ rD = 3 │ rA = 1 │ SIMM = 0x0010             │   addi r3, r1, 16
  └──────────┴────────┴────────┴───────────────────────────┘
   (bit numbering here is little-endian style; PowerPC manuals number from the left)
```

Fixed width makes decoding simple, which helps both the
[interpreter](../guide/wiiu_term.md#t11-1) and the [JIT](../guide/wiiu_term.md#t11-2). Opcode
1 is unused on Espresso; shocora uses it as its "call into HLE" instruction (chapter 1).

## 5.4 Paired singles and GQRs: the 3D maths helper

```text
   psq_l f1, 0(r4), 0, qr2        load two values through quantization register 2

   memory (s16, s16)            GQR2: type = s16, scale = 2^-8          f1
   ┌────────┬────────┐          ┌─────────────────────────────┐   ┌─────────┬─────────┐
   │ 0x0180 │ 0xFF00 │  ──────► │ convert to float, × 1/256   │──►│  1.5    │  -1.0   │
   └────────┴────────┘          └─────────────────────────────┘   └─────────┴─────────┘
                                                                     ps0       ps1
   ps_madd f0, f1, f2, f3       both lanes at once: f0 = f1 × f2 + f3
```

- [Paired singles](../guide/wiiu_term.md#t2-10) treat each float register as two lanes.
- A [GQR](../guide/wiiu_term.md#t2-11) tells `psq_l` / `psq_st` how to pack values: plain
  float, or 8/16-bit integers with a power-of-two scale. Games store vertices and animation
  data compressed this way.
- Results must be bit-exact. Rounding mode (FPSCR), the single-precision rounding of `ps1`,
  and denormal handling all show up as objects sliding or jittering if wrong.

## 5.5 Caches and the instructions that manage them

```text
   dcbz  addr   zero a 32-byte line without reading memory first (fast memset)
   dcbf  addr   write the line back and drop it           ┐ used before the GPU
   dcbst addr   write the line back, keep it              ┘ reads CPU data
   dcbi  addr   drop the line without writing             ─ after the GPU wrote
   icbi  addr   drop an instruction-cache line            ─ after writing code
```

A [cache line](../guide/wiiu_term.md#t3-8) is 32 bytes. For an emulator, `dcbz` is the one that
changes memory (it zeroes 32 bytes); the others are hints. `icbi` matters: it says code
changed, so translated code for that address must be thrown away.

## 5.6 The locked cache: a scratchpad inside the CPU

```text
   L1 data cache (32 KiB)
   ┌──────────────────────┬──────────────────────┐
   │ normal cache 16 KiB  │ locked cache 16 KiB  │◄─ LCAlloc, then DMA:
   └──────────────────────┴──────────────────────┘     MEM2 ──LCLoadDMABlocks──► LC
                                                        LC ──LCStoreDMABlocks──► MEM2
```

With the [locked cache](../guide/wiiu_term.md#t2-12) enabled, half of the L1 data cache becomes
a private 16 KiB memory per core, at its own effective address. Games use it for hot loops over
data streamed in by DMA. An emulator maps a small memory region per core and performs the DMA
as a copy.

## 5.7 Time: time base and decrementer

```text
   bus clock 248.625 MHz ÷ 4 = 62.15625 MHz
                     │
     ┌───────────────┴────────────────┐
     ▼                                ▼
  time base (64-bit, counts up)    decrementer (32-bit, counts down)
  read by OSGetTime, OSGetTick     reaches 0 ─► interrupt ─► Cafe OS fires
  "what time is it?"               the next OSAlarm / thread time slice
```

- The [time base](../guide/wiiu_term.md#t2-13) is the console's clock. Games compute
  frame times, timeouts and animation from it.
- The [decrementer](../guide/wiiu_term.md#t2-14) raises an
  [interrupt](../guide/wiiu_term.md#t4-27) when it reaches zero; Cafe OS uses it for
  [alarms](../guide/wiiu_term.md#t4-24) and preemption.
- An emulator does not need to match the 1.24 GHz core clock, only this 62 MHz time. If
  emulated time runs ahead of real time, the game runs fast; if behind, it slows down.

## 5.8 Three cores together

```text
   core 0             core 1             core 2
     │ OSSendMessage    │                  │
     │──── writes ─────►│ message queue    │
     │                  │ (in MEM2)        │
     │ IPI (inter-processor interrupt) ───►│ "reschedule now"
     │                  │                  │
   atomic updates: lwarx / stwcx. pairs (load-reserve, store-conditional)
```

- All cores share memory. Locks and counters use the `lwarx` / `stwcx.` pair: the store only
  succeeds if nobody touched the reserved line in between.
- When one core makes a thread runnable on another, Cafe OS sends an inter-processor
  interrupt so that core reschedules at once.
- In shocora's [multi-core mode](../guide/wiiu_term.md#t11-7) each guest core is a host
  thread; HLE calls take a shared [lock](../guide/wiiu_term.md#t11-10), and "wake that core"
  is a condition-variable signal instead of an IPI.

## 5.9 How shocora runs a core

```text
   guest PC
     │
     ▼
   block cache hit? ──yes──► run translated host code ─────────────┐
     │ no                                                          │
     ▼                                                             │
   interpret this block now ─► count it ─► hot? ─► queue for       │
                                                  Cranelift (bg)   │
                                                                   ▼
   exits: end of time slice · HLE call (opcode 1) · interrupt pending · trap
```

The [interpreter](../guide/wiiu_term.md#t11-1) runs code the first times it is seen;
[hot code](../guide/wiiu_term.md#t11-5) is compiled in the background by
[Cranelift](../guide/wiiu_term.md#t11-3) into a [code cache](../guide/wiiu_term.md#t11-6).
Translated [blocks](../guide/wiiu_term.md#t11-4) jump straight to each other until something
needs the OS.
