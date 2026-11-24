# 4. Memory Map

Two kinds of [address](../guide/wiiu_term.md#t3-1) exist on the Wii U. The CPU's programs use
**virtual** ([effective](../guide/wiiu_term.md#t3-3)) addresses, translated by the CPU's MMU.
The GPU, the ARM side and the memory controller use **[physical](../guide/wiiu_term.md#t3-4)**
addresses. A game only ever writes effective addresses, but whenever it hands a pointer to the
GPU (a texture, a command buffer) the OS converts it to a physical one.

## 4.1 The two views

```text
   what the game sees (effective)                 what the chips see (physical)
   ──────────────────────────────                 ─────────────────────────────
 0x0100_0000 ┌─────────────────────┐
             │ code (RPX + RPLs)   │─────────┐
 0x0E00_0000 ├─────────────────────┤         │
             │ (shocora: HLE area) │         │    0x0000_0000 ┌──────────────────────┐
 0x1000_0000 ├─────────────────────┤         │                │ MEM1   32 MiB eDRAM  │◄─┐
             │ MEM2 data, heaps,   │──┐      │    0x0200_0000 └──────────────────────┘  │
             │ stacks   (1 GiB)    │  │      │                                          │
 0x5000_0000 └─────────────────────┘  │      │    0x1000_0000 ┌──────────────────────┐  │
                                      │      └───►            │ MEM2   2 GiB DDR3    │  │
 0xA000_0000 ┌─────────────────────┐  │                       │   system half        │  │
             │ overlay arena (opt.)│  │           0x5000_0000 │   ─ ─ ─ ─ ─ ─ ─ ─ ─  │  │
 0xBC00_0000 └─────────────────────┘  └──────────────────────►│   game half (1 GiB)  │  │
 0xE000_0000 ┌─────────────────────┐                          │                      │  │
             │ foreground bucket   │─────────────────────────►│                      │  │
 0xE400_0000 └─────────────────────┘                          └──────────────────────┘  │
 0xE800_0000 ┌─────────────────────┐                                                    │
             │ tiling aperture     │ (linear window onto tiled surfaces)               │
 0xEA00_0000 └─────────────────────┘                                                    │
 0xF400_0000 ┌─────────────────────┐                                                    │
             │ MEM1     (32 MiB)   │────────────────────────────────────────────────────┘
 0xF600_0000 ├─────────────────────┤
             │ loader data         │
 0xF800_0000 ├─────────────────────┤
             │ shared (fonts…)     │
 0xFA00_0000 └─────────────────────┘
 0xFFC0_0000 ┌─────────────────────┐
             │ locked cache window │ (per core, see 5.6)
             └─────────────────────┘
```

The game's MEM2 window at `0x1000_0000` is backed by physical `0x5000_0000`; MEM1 at
`0xF400_0000` is backed by physical `0x0000_0000`. shocora keeps the same numbers
(`core/mem_addr`), because games and [graphic packs](../guide/wiiu_term.md#t11-12) hard-code
them.

## 4.2 Regions, one by one

```text
 ┌──────────────────┬──────────────┬──────────┬───────────────────────────────────────────┐
 │ Region           │ Effective    │ Size     │ Holds                                     │
 ├──────────────────┼──────────────┼──────────┼───────────────────────────────────────────┤
 │ code area        │ 0x0100_0000  │ ≈208 MiB │ .text of the RPX and every RPL            │
 │   modules from   │ 0x0200_0000  │          │ (patches and Cemu use these addresses)    │
 │ MEM2 (app)       │ 0x1000_0000  │ 1 GiB    │ .data/.bss, default heap, thread stacks   │
 │ overlay arena    │ 0xA000_0000  │ 448 MiB  │ extra memory borrowed from the system,    │
 │                  │              │          │ only after OSEnableOverlayArena           │
 │ foreground bucket│ 0xE000_0000  │ 64 MiB   │ memory owned by whoever is in front       │
 │ tiling aperture  │ 0xE800_0000  │ 32 MiB   │ linear view of a tiled surface            │
 │ MEM1             │ 0xF400_0000  │ 32 MiB   │ render targets, depth, hot GPU data       │
 │ loader           │ 0xF600_0000  │ 32 MiB   │ RPL metadata, system .rodata              │
 │ shared           │ 0xF800_0000  │ 32 MiB   │ read-only data such as system fonts       │
 │ locked cache     │ 0xFFC0_0000  │ 16 KiB×3 │ the cache-as-scratchpad of each core      │
 └──────────────────┴──────────────┴──────────┴───────────────────────────────────────────┘
```

- **Code area.** Executable and read-only. The [loader](../guide/wiiu_term.md#t4-31) places
  the main [RPX](../guide/wiiu_term.md#t4-6) and its [RPLs](../guide/wiiu_term.md#t4-7) here.
  In shocora this is also where the [JIT](../guide/wiiu_term.md#t11-2) looks for code to
  translate, and writes here invalidate translated [blocks](../guide/wiiu_term.md#t11-4).
- **MEM2.** Where nearly everything of the game lives. `cos.xml` in the title's `code/`
  folder says how much of it the game may use (chapter 10).
- **[Foreground bucket](../guide/wiiu_term.md#t3-7).** Belongs to the application in front.
  When the user opens the [HOME Menu](../guide/wiiu_term.md#t4-30), the game must save what it
  keeps there, because the menu takes it over.
- **[Aperture](../guide/wiiu_term.md#t3-9).** The GPU stores surfaces
  [tiled](../guide/wiiu_term.md#t5-21); an aperture lets the CPU read and write one as if it
  were a plain row-by-row image.
- **[Locked cache (LC)](../guide/wiiu_term.md#t2-12).** Half of a core's L1 data cache,
  usable as a tiny private scratch memory with DMA in and out.

## 4.3 Inside MEM2: how a game carves up its gigabyte

```text
 0x1000_0000 ┌──────────────────────────────┐
             │ .data / .bss of RPX + RPLs   │  placed by the loader
             ├──────────────────────────────┤
             │ main thread stack            │  size from cos.xml
             ├──────────────────────────────┤
             │                              │
             │ default heap (expanded heap) │  MEMAllocFromDefaultHeap / malloc
             │   ├ game's own sub-heaps     │  frame heaps, unit heaps...
             │   ├ textures, models, audio  │
             │   └ command buffers          │
             │                              │
 end of app  └──────────────────────────────┘
```

A [heap](../guide/wiiu_term.md#t3-6) is a region the game manages itself with Cafe OS's
`MEM*` functions: an *expanded heap* for general allocations, *frame heaps* for "allocate many,
free all at once" (per level, per frame), and *unit heaps* for fixed-size objects. All of them
keep their bookkeeping inside guest memory, so an emulator must lay those headers out exactly
as the console does; some games walk them directly.

## 4.4 Why the GPU needs physical addresses

```text
   game                       Cafe OS (gx2)                    GPU7
   ────                       ─────────────                    ────
   tex.image = 0x2034_0000 ──► OSEffectiveToPhysical ──► reg SQ_TEX_RESOURCE.base
                               = 0x6034_0000                   = 0x6034_0000 >> 8
```

GPU registers hold physical addresses (often shifted right by 8 because surfaces are
256-byte aligned). An emulator that reads GPU state must translate back to find the bytes in
its own copy of guest memory. shocora does this in `wiiu_mem_addr::effective_to_physical` and
its inverse.

## 4.5 Byte order everywhere

All of memory is [big-endian](../guide/wiiu_term.md#t2-8). A `u32` of `0x1234_5678` is stored
as the bytes `12 34 56 78`. The GPU reads vertex and index data with a per-buffer "endian
swap" setting, so the same memory can feed both the CPU and the GPU correctly. On a
little-endian PC, every guest load and store goes through a byte swap.

## 4.6 Coherency: who sees a write, and when

```text
   CPU core ──write──► L1/L2 cache ──(flush: DCFlushRange)──► MEM2 ──► GPU reads it
   GPU ──────write──────────────────────────────────────────► MEM2 ──(invalidate)──► CPU reads
```

The CPU [caches](../guide/wiiu_term.md#t2-5) are coherent between the three cores but not with
the GPU. Games therefore flush a range after filling it for the GPU, and invalidate before
reading what the GPU wrote. shocora's memory is a single coherent array, so it uses those
flushes only as hints ("this texture changed, upload it again").
