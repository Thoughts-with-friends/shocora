# 7.2 PM4 Packets

[← 7.1 GX2 and the command flow](07_1_gx2_command_flow.md) · [7. Latte Graphics](07_gpu_latte.md) · next: [7.3 Registers and GPU state →](07_3_registers_state.md)

[PM4](../guide/wiiu_term.md#t5-3) is the command language of AMD's Radeon GPUs of the R600/R700
generation, and therefore of [Latte](../guide/wiiu_term.md#t2-16). A
[command buffer](../guide/wiiu_term.md#t5-2) is nothing but a list of 32-bit words forming PM4
packets. The GPU's **command processor** (CP) reads them one after another.

## 7.2.1 Packet types

```text
   bits 31..30 of the first word = packet type

   type 0  ┌──┬──────────────┬────────────────────────┐   "write N registers starting at R"
           │00│ count (14 b) │ register index (16 b)  │   (rare on the Wii U)
           └──┴──────────────┴────────────────────────┘

   type 2  ┌──┬─────────────────────────────────────────┐   filler, does nothing
           │10│ (ignored)                               │   (used to pad buffers)
           └──┴─────────────────────────────────────────┘

   type 3  ┌──┬──────────────┬──────────────┬───────┬─┐   "operation with N+1 data words"
           │11│ count (14 b) │ opcode (8 b) │       │p│   almost everything GX2 writes
           └──┴──────────────┴──────────────┴───────┴─┘
             count = number of data words − 1;  p = predicate bit
```

Words are [big-endian](../guide/wiiu_term.md#t2-8) in memory, like everything on the Wii U.

## 7.2.2 Reading a type-3 packet

```text
   word  value         meaning
   ────  ─────         ───────
   0     0xC002_6900   type 3, count 2 (= 3 data words), opcode 0x69 SET_CONTEXT_REG
   1     0x0000_0010   register offset 0x10 inside the context space
                       → 0xA000 + 0x10 = CB_COLOR0_BASE
   2     0x0030_4000   value for CB_COLOR0_BASE    (surface address >> 8)
   3     0x0000_0000   value for CB_COLOR0_BASE+1  (consecutive registers)

   then the next packet starts at word 4
```

The CP walks packets like this: read header, take `count + 1` data words, act, move on. An
unknown opcode can still be skipped because its length is in the header, which is how an
emulator survives packets it does not understand.

## 7.2.3 The opcodes GX2 uses

```text
 ┌──────┬────────────────────────┬───────────────────────────────────────────────────────────┐
 │ op   │ name                   │ what it does                                              │
 ├──────┼────────────────────────┼───────────────────────────────────────────────────────────┤
 │ 0x10 │ NOP                    │ nothing; GX2 also hides markers and debug tags in it      │
 │ 0x68 │ SET_CONFIG_REG         │ write config registers (primitive type, global setup)     │
 │ 0x69 │ SET_CONTEXT_REG        │ write context registers (targets, blend, depth, shaders)  │
 │ 0x6A │ SET_ALU_CONST          │ write shader constants (vec4 uniforms)                    │
 │ 0x6B │ SET_BOOL_CONST         │ write boolean constants (static branches)                 │
 │ 0x6C │ SET_LOOP_CONST         │ write loop constants (loop counts)                        │
 │ 0x6D │ SET_RESOURCE           │ describe a texture / vertex buffer / uniform block        │
 │ 0x6E │ SET_SAMPLER            │ describe a sampler (filter, wrap, LOD)                    │
 │ 0x6F │ SET_CTL_CONST          │ control constants (e.g. base vertex / instance)           │
 ├──────┼────────────────────────┼───────────────────────────────────────────────────────────┤
 │ 0x2A │ INDEX_TYPE             │ 16- or 32-bit indices, byte swap                          │
 │ 0x2F │ NUM_INSTANCES          │ instance count for the next draw                          │
 │ 0x2D │ DRAW_INDEX_AUTO        │ draw without indices (0, 1, 2, …)                         │
 │ 0x27 │ DRAW_INDEX_2           │ draw with an index buffer in memory                       │
 │ 0x2E │ DRAW_INDEX_IMMD        │ draw with indices inside the packet                       │
 ├──────┼────────────────────────┼───────────────────────────────────────────────────────────┤
 │ 0x32 │ INDIRECT_BUFFER        │ run another command buffer, then come back (display list) │
 │ 0x43 │ SURFACE_SYNC           │ flush / invalidate GPU caches for an address range        │
 │ 0x46 │ EVENT_WRITE            │ pipeline events (cache flush, query begin/end, …)         │
 │ 0x47 │ EVENT_WRITE_EOP        │ at end of pipe: write a value (timestamp) + interrupt     │
 │ 0x3D │ MEM_WRITE              │ write a value to memory when reached                      │
 │ 0x3C │ WAIT_REG_MEM           │ wait until a memory word / register matches a value       │
 │ 0x39 │ MEM_SEMAPHORE          │ GPU-side semaphore signal / wait                          │
 │ 0x34 │ STRMOUT_BUFFER_UPDATE  │ stream-out buffer offsets (save / restore)                │
 │ 0x20 │ SET_PREDICATION        │ skip following draws depending on a query result          │
 │ 0x28 │ CONTEXT_CONTROL        │ which register groups are shadowed (context state, 7.1.5) │
 └──────┴────────────────────────┴───────────────────────────────────────────────────────────┘
```

Groups: the first block sets [state](07_3_registers_state.md), the second
[draws](../guide/wiiu_term.md#t5-5), the third controls flow and synchronisation
([7.7](07_7_display_sync.md)).

## 7.2.4 A draw, as packets

```text
   C0 01 68 00 │ 00 00 02 56 │ 00 00 00 04        SET_CONFIG_REG VGT_PRIMITIVE_TYPE = 4 (triangle list)
   C0 00 2A 00 │ 00 00 00 00                      INDEX_TYPE = 16-bit, swap as needed
   C0 00 2F 00 │ 00 00 00 01                      NUM_INSTANCES = 1
   C0 04 27 00 │ 00 00 0F FF │ 20 45 60 00 │      DRAW_INDEX_2 (5 data words): max index,
               │ 00 00 00 00 │ 00 00 01 20 │      index address low / high, index count 0x120,
               │ 00 00 00 00                      draw initiator (source = DMA'd indices)

   offsets in SET_CONFIG_REG are relative to the config space: 0x2256 − 0x2000 = 0x256
```

## 7.2.5 INDIRECT_BUFFER: calling a display list

```text
   main buffer                               display list (game memory)
   ─────────────                             ──────────────────────────
   …
   INDIRECT_BUFFER  addr_lo ───────────────► [SET_RESOURCE …]
                    addr_hi                  [SET_CONTEXT_REG …]
                    size (words)             [DRAW_INDEX_AUTO …]
   ◄──────────────────────────────── returns after `size` words
   …next packet
```

A [display list](../guide/wiiu_term.md#t5-4) runs as if its packets were pasted in place.
Lists may call other lists. The address is [physical](../guide/wiiu_term.md#t3-4), so an
emulator translates it back to find the words ([4.4](04_memory_map.md#44-why-the-gpu-needs-physical-addresses)).

## 7.2.6 shocora's private packets

```text
 ┌──────┬────────────────────────────────────┬────────────────────────────────────────────┐
 │ op   │ name (shocora only)                │ why it exists                              │
 ├──────┼────────────────────────────────────┼────────────────────────────────────────────┤
 │ 0xE0 │ HLE_CLEAR                          │ GX2ClearColor/Depth as one command         │
 │ 0xE1 │ HLE_COPY_COLOR_BUFFER_TO_SCAN_BUFFER│ the copy into a scan buffer               │
 │ 0xE2 │ HLE_SWAP_SCAN_BUFFERS              │ the flip request                           │
 │ 0xE3 │ HLE_CONTEXT_STATE                  │ load a saved GX2ContextState               │
 │ 0xE4 │ HLE_COPY_SURFACE                   │ GX2CopySurface (format/tiling aware)       │
 │ 0xE5 │ HLE_SET_TARGET                     │ bind a render target in one step           │
 │ 0xFA │ HLE_BEGIN_OCCLUSION_QUERY          │ occlusion queries                          │
 │ 0xFB │ HLE_END_OCCLUSION_QUERY            │                                            │
 └──────┴────────────────────────────────────┴────────────────────────────────────────────┘
```

On a real console, `GX2ClearColor` is implemented by drawing a rectangle with special shaders,
and a copy is a draw too. Because shocora replaces GX2 with Rust, it can emit one high-level
packet instead, which is faster to execute and easier to map onto the host GPU. These opcodes
are unused by AMD in this range, so they never collide with packets a game writes itself
(`core/gpu/src/command/opcode.rs`).

## 7.2.7 Summary

```text
   type-3 header = [11][count][opcode] ─► count+1 data words follow
   SET_* = write registers · DRAW_* = start work · INDIRECT_BUFFER = call a list
   EVENT_WRITE_EOP / MEM_WRITE / WAIT_REG_MEM = synchronisation (7.7)
```
