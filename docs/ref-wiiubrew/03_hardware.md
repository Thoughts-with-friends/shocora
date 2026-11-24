# 3. Hardware Overview

The Wii U is three computers sharing one box: a PowerPC [CPU](../guide/wiiu_term.md#t2-1)
that runs the game, a [GPU](../guide/wiiu_term.md#t2-15) that draws, and a small ARM
processor that owns every device. The game only ever runs on the first one. Everything else
it reaches through memory or through requests to the ARM side.

## 3.1 The block diagram

```text
                    ┌──────────────────────── Multi-chip module (MCM) ─────────────────────────┐
                    │                                                                         │
                    │   ┌──────────── Espresso (IBM) ───────────┐                             │
                    │   │  core 0        core 1        core 2   │   3 × PowerPC 750-class     │
                    │   │  L2 512 KiB    L2 2 MiB      L2 512 KiB│   1.243125 GHz              │
                    │   └───────────────────┬───────────────────┘                             │
                    │                       │ 60x-style bus (248.625 MHz)                     │
                    │   ┌───────────────────▼──────────── Latte (AMD/Nintendo) ─────────────┐ │
                    │   │                                                                   │ │
                    │   │  ┌────────────┐  ┌──────────────┐  ┌─────────────┐  ┌──────────┐  │ │
                    │   │  │ GPU7       │  │ MEM1         │  │ Starbuck    │  │ DSP      │  │ │
                    │   │  │ R7xx-class │  │ 32 MiB eDRAM │  │ ARM9 core   │  │ (audio)  │  │ │
                    │   │  │ shaders    │  │ fast, on-die │  │ runs IOSU   │  │          │  │ │
                    │   │  └─────┬──────┘  └──────────────┘  └──────┬──────┘  └──────────┘  │ │
                    │   │        │   display controller (TV + GamePad)  │ AES/SHA, I/O ctrl  │ │
                    │   └────────┼──────────────────────────────────────┼────────────────────┘ │
                    └────────────┼──────────────────────────────────────┼──────────────────────┘
                                 │                                      │
              ┌──────────────────▼───┐                 ┌────────────────▼─────────────────────┐
              │ MEM2: 2 GiB DDR3     │                 │ eMMC "MLC" (8/32 GB) · NAND "SLC"     │
              │ 1 GiB for the game,  │                 │ disc drive · USB · SD · Wi-Fi ·       │
              │ 1 GiB for the system │                 │ Bluetooth (Wii remotes) · GamePad link │
              └──────────────────────┘                 └───────────────────────────────────────┘
```

Read it from the top: [Espresso](../guide/wiiu_term.md#t2-2) is the CPU package with three
[cores](../guide/wiiu_term.md#t2-3). [Latte](../guide/wiiu_term.md#t2-16) is one big chip that
holds the GPU, 32 MiB of embedded memory ([MEM1](../guide/wiiu_term.md#t2-20)), the ARM
security processor ([Starbuck](../guide/wiiu_term.md#t2-22)) and the audio DSP. The main RAM
([MEM2](../guide/wiiu_term.md#t2-21)) and all storage hang off Latte, not off the CPU.

## 3.2 Who does what

```text
  ┌────────────────────┬──────────────────────────────┬─────────────────────────────────────┐
  │ Part               │ Runs                         │ In shocora                          │
  ├────────────────────┼──────────────────────────────┼─────────────────────────────────────┤
  │ Espresso core 0..2 │ the game's PowerPC code and  │ interpreter + Cranelift JIT,        │
  │                    │ the Cafe OS libraries        │ libraries replaced by Rust (HLE)    │
  │ GPU7 (in Latte)    │ PM4 command buffers, R600-   │ PM4 parser + shader translator,     │
  │                    │ family shaders               │ drawn through wgpu / OpenGL         │
  │ Starbuck (ARM9)    │ IOSU: files, accounts, USB,  │ not run; IOSU devices answered in   │
  │                    │ network, security            │ Rust (wiiu_iosu, HLE IPC)           │
  │ DSP                │ audio help for AX            │ AX mixing done in Rust              │
  │ Display controller │ scan-out to TV and GamePad   │ the front end presents both screens │
  └────────────────────┴──────────────────────────────┴─────────────────────────────────────┘
```

This split is the reason [HLE (high-level emulation)](../guide/wiiu_term.md#t4-3) works on the
Wii U: the game never sees the ARM side directly, only the answers to its requests, so the
emulator can produce those answers any way it likes (see chapter 8).

## 3.3 The CPU package: three unequal cores

```text
   core 0                 core 1 (main)              core 2
  ┌─────────────┐        ┌─────────────────┐        ┌─────────────┐
  │ L1 I 32 KiB │        │ L1 I 32 KiB     │        │ L1 I 32 KiB │
  │ L1 D 32 KiB │        │ L1 D 32 KiB     │        │ L1 D 32 KiB │
  │ L2  512 KiB │        │ L2    2 MiB     │        │ L2  512 KiB │
  └──────┬──────┘        └────────┬────────┘        └──────┬──────┘
         └────────────── coherent bus ────────────────────┘
```

- Core 1 starts the game and usually runs the main loop and the drawing thread. It has four
  times the [cache](../guide/wiiu_term.md#t2-5) of the other two for that reason.
- Cores 0 and 2 get worker threads: physics, streaming, audio, decompression. A game picks
  the core of each [thread](../guide/wiiu_term.md#t4-10) itself
  ([Affinity](../guide/wiiu_term.md#t4-13)).
- The cores are slow by PC standards (an in-order-ish 750 design, no SIMD beyond
  [paired singles](../guide/wiiu_term.md#t2-10)), but an emulator spends many host
  instructions per guest instruction, so all three still need a fast host core each.

Chapter 5 goes inside a core.

## 3.4 Latte: more than a GPU

```text
  ┌────────────────────────────────── Latte ──────────────────────────────────┐
  │                                                                           │
  │  ┌─ GPU7 ───────────────────┐   ┌─ MEM1 ──────────┐   ┌─ Starbuck ─────┐  │
  │  │ command processor (CP)   │   │ 32 MiB eDRAM    │   │ ARM926-class   │  │
  │  │   reads PM4 rings        │◄──│ render targets, │   │ IOSU kernel +  │  │
  │  │ shader cores (VLIW5)     │   │ depth buffers   │   │ processes      │  │
  │  │ texture units, ROPs      │──►│                 │   │ crypto engines │  │
  │  └──────────────────────────┘   └─────────────────┘   └───────┬────────┘  │
  │                                                               │ IPC regs  │
  │  ┌─ Display controller ─────┐   ┌─ Memory controller ─┐       │           │
  │  │ TV scan-out (HDMI)       │   │ MEM2 DDR3 access    │◄──────┘           │
  │  │ GamePad frames → encoder │   │ for CPU, GPU, ARM   │                   │
  │  └──────────────────────────┘   └─────────────────────┘                   │
  │  ┌─ Wii-mode legacy blocks (vWii) ─────────────────────────────────────┐   │
  │  │ "MEM0" SRAM, Hollywood-compatible registers (unused by Wii U games) │   │
  │  └─────────────────────────────────────────────────────────────────────┘   │
  └───────────────────────────────────────────────────────────────────────────┘
```

- The GPU is an AMD R7xx-family design. Its shader code is the
  [R600 / R700 ISA](../guide/wiiu_term.md#t2-18), and it is fed with
  [PM4](../guide/wiiu_term.md#t5-3) packets, just like the Radeon HD 4000 series of PCs.
- [MEM1](../guide/wiiu_term.md#t2-20) is small and fast. Games put their render targets and
  [depth buffers](../guide/wiiu_term.md#t5-18) there; textures and models live in MEM2.
- [Starbuck](../guide/wiiu_term.md#t2-22) is the gatekeeper: storage, network, USB and
  crypto keys are only reachable through it ([IOSU](../guide/wiiu_term.md#t10-1)).
- The legacy blocks exist for [vWii](../guide/wiiu_term.md#t2-32) (Wii backwards
  compatibility) and play no part when a Wii U game runs.

## 3.5 Outside the main chips

```text
   ┌──────────── console ─────────────┐            ┌──────── GamePad (DRC) ────────┐
   │ Latte display ctrl ─► H.264 enc ─┼── 5 GHz ──►│ decoder ─► 854×480 screen     │
   │ Latte audio ────────► (in stream)│   Wi-Fi    │ speakers, mic, camera         │
   │ Bluetooth ◄──────────────────────┼── 2.4 GHz ─┤                               │
   │   Wii remotes, Pro Controller    │            │ buttons, sticks, touch,       │
   │ HDMI ─► TV (up to 1080p)         │◄───────────┤ gyro/accel/magnetometer, NFC │
   └──────────────────────────────────┘   input    └───────────────────────────────┘
```

- The [GamePad](../guide/wiiu_term.md#t2-26) ([DRC](../guide/wiiu_term.md#t2-27)) is a
  wireless screen: the console renders its picture, compresses it and streams it. Input
  comes back over the same link.
- [Wii remotes](../guide/wiiu_term.md#t2-30) and the
  [Pro Controller](../guide/wiiu_term.md#t2-29) use Bluetooth.
- Storage: the internal eMMC ([NAND / MLC](../guide/wiiu_term.md#t2-23)) holds installed
  games and saves; a separate small NAND ([SLC](../guide/wiiu_term.md#t2-24)) holds the
  system. Keys live in [SEEPROM / OTP](../guide/wiiu_term.md#t2-25).

## 3.6 What an emulator must reproduce, and what it may skip

```text
  must match exactly                      may be replaced freely
  ───────────────────                     ────────────────────────
  PowerPC instruction results             how IOSU produces an answer
  guest memory contents and layout        how pixels reach your monitor
  the time the game measures              the DSP's internals
  GPU results the game reads back         the GamePad radio link
  (queries, copies to CPU memory)         crypto (titles arrive decrypted)
```

The left column is everything the game can observe. The right column is hidden behind an
API, so shocora answers at the API level instead (chapter 1).
