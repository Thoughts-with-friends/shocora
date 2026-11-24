# 7. Latte Graphics (GX2)

Games draw through [GX2](../guide/wiiu_term.md#t5-1), Cafe OS's graphics library. GX2 is thin:
it writes [PM4](../guide/wiiu_term.md#t5-3) packets into a
[command buffer](../guide/wiiu_term.md#t5-2), and the GPU's command processor executes them.
If you know Direct3D 9/10 or OpenGL, GX2 will feel familiar; the difference is that the game
also sees, and sometimes builds, the raw packets.

## 7.1 The pipeline from a function call to the screen

```text
   game                    gx2 (in the game process)          Latte GPU7
   ────                    ─────────────────────────          ──────────
   GX2SetShaders ─────┐
   GX2SetAttribBuffer ┤    append PM4 packets
   GX2SetPixelTexture ┼──► ┌───────────────────────────┐
   GX2SetColorBuffer ─┤    │ SET_CONTEXT_REG ...       │
   GX2Draw ───────────┘    │ SET_RESOURCE ...          │      command processor
                           │ DRAW_INDEX_AUTO           │ ───► reads packets in order,
   GX2SwapScanBuffers ───► │ (flip request)            │      writes GPU registers,
                           └───────────────────────────┘      launches draws
                                     │ GX2Flush / buffer full            │
                                     ▼                                   ▼
                              ring buffer handed to the GPU      shader cores ─► MEM1 targets
                                                                         │
                                                              copy to scan buffer ─► TV / GamePad
```

- A [draw call](../guide/wiiu_term.md#t5-5) is only the last packet; everything before it
  sets state (shaders, buffers, textures, blend, depth).
- The GPU runs behind the CPU. GX2 lets the game wait for it
  ([`GX2DrawDone`](../guide/wiiu_term.md#t5-33)) or read
  [timestamps](../guide/wiiu_term.md#t5-32) to see how far it has got.

## 7.2 PM4 packets

```text
   type-3 packet header (32 bits)                         then `count + 1` data words
   ┌──┬──────────────┬────────────────┬───┬─────┐
   │11│ count (14 b) │ opcode (8 b)   │...│ prd │       e.g. SET_CONTEXT_REG
   └──┴──────────────┴────────────────┴───┴─────┘            word 0: register offset
    type                                                     word 1..n: values

   common opcodes
   SET_CONFIG_REG / SET_CONTEXT_REG / SET_ALU_CONST / SET_RESOURCE / SET_SAMPLER
   DRAW_INDEX_AUTO / DRAW_INDEX_2 / DRAW_INDEX_IMMD
   INDIRECT_BUFFER_PRIV  (call a display list)
   EVENT_WRITE / EVENT_WRITE_EOP  (timestamps, cache flushes)
   SURFACE_SYNC, WAIT_REG_MEM, MEM_WRITE, NOP
```

A [display list](../guide/wiiu_term.md#t5-4) is a prerecorded block of packets the game calls
with `INDIRECT_BUFFER_PRIV`, like a function call for the GPU. Games such as BotW build many of
them once and replay them each frame.

```text
   main ring ── ... ── INDIRECT_BUFFER ──► display list A ── ... ── (returns)
                     └─ INDIRECT_BUFFER ──► display list B
```

## 7.3 Shaders: four stages, one ISA

```text
   vertex buffers
        │
        ▼
   ┌───────────────┐   ┌───────────────┐   ┌────────────────┐   ┌───────────────┐
   │ fetch shader  │──►│ vertex shader │──►│ geometry shader│──►│ rasterizer    │
   │ (reads and    │   │ (positions,   │   │ (optional;     │   │ (triangles to │
   │  unpacks      │   │  per-vertex   │   │  can stream-   │   │  pixels)      │
   │  attributes)  │   │  outputs)     │   │  out to memory)│   └───────┬───────┘
   └───────────────┘   └───────────────┘   └────────────────┘           ▼
                                                               ┌───────────────┐
                                                               │ pixel shader  │──► color /
                                                               │ (colors)      │    depth targets
                                                               └───────────────┘
```

- All stages are machine code in the [R600 / R700 ISA](../guide/wiiu_term.md#t2-18): control
  flow (CF) instructions that call ALU clauses (VLIW: up to five operations per bundle) and
  texture/vertex-fetch clauses.
- The [fetch shader](../guide/wiiu_term.md#t5-8) is a small program that turns raw vertex
  memory into shader inputs; on PC the input layout does this.
- [Uniforms](../guide/wiiu_term.md#t5-12) arrive as ALU constants or uniform blocks.
- [Stream-out](../guide/wiiu_term.md#t5-13) lets a stage write its results back to memory.
- The PC GPU cannot run this code, so the emulator translates each
  [shader](../guide/wiiu_term.md#t2-17) to WGSL/GLSL and builds a
  [pipeline](../guide/wiiu_term.md#t5-37) for each state combination, keeping both in a
  [shader / pipeline cache](../guide/wiiu_term.md#t5-38).

## 7.4 Surfaces, formats and tiling

```text
   GX2Surface
   ┌────────────────────────────────────────────────────────────┐
   │ dim (2D, 3D, cube, array) · width · height · depth · mips  │
   │ format (e.g. R8G8B8A8_UNORM, BC3, D24_S8)                  │
   │ aa (MSAA samples) · use (texture / color / depth / scan)   │
   │ tileMode · swizzle · pitch · image ptr · mipmap ptr        │
   └────────────────────────────────────────────────────────────┘
```

- A [surface](../guide/wiiu_term.md#t5-16) is any image: [texture](../guide/wiiu_term.md#t5-14),
  [render target](../guide/wiiu_term.md#t5-17), [depth buffer](../guide/wiiu_term.md#t5-18).
- [Formats](../guide/wiiu_term.md#t5-19) include block-compressed
  [BC1–BC5](../guide/wiiu_term.md#t5-20), which modern PC GPUs also understand.
- [Mipmaps](../guide/wiiu_term.md#t5-15) are stored after the base level, each with its own
  offset.

```text
   linear (row by row)                    2D tiled (macro tiles of 8×8 micro tiles)
   ┌──┬──┬──┬──┬──┬──┬──┬──┐               ┌─────────┬─────────┐
   │ 0│ 1│ 2│ 3│ 4│ 5│ 6│ 7│               │ tile 0  │ tile 1  │   each micro tile = 8×8 pixels
   ├──┼──┼──┼──┼──┼──┼──┼──┤      ──►      │ ▓▓░░▓▓░░│ bank/pipe│   stored contiguously; tiles
   │ 8│ 9│10│11│12│13│14│15│               │ ░░▓▓░░▓▓│ swizzled │   spread over memory banks
   └──┴──┴──┴──┴──┴──┴──┴──┘               └─────────┴─────────┘   for bandwidth
```

[Tiling](../guide/wiiu_term.md#t5-21) reorders pixels so that a small square of the image sits
together in memory ([micro / macro tiles](../guide/wiiu_term.md#t5-22),
[swizzle](../guide/wiiu_term.md#t5-23), [pitch](../guide/wiiu_term.md#t5-24)). The exact
addressing comes from AMD's [AddrLib](../guide/wiiu_term.md#t5-25). An emulator must untile
when uploading a texture to the PC GPU, and tile again when the game reads a GPU result back
through the CPU.

## 7.5 Render targets, MSAA and copies

```text
   draw ──► color buffer (MEM1, maybe 4× MSAA) ──resolve──► single-sample texture
                                                 │
                     GX2CopySurface ────────────┤ (format change, tiling change,
                                                 │  mip generation, readback to MEM2)
                     GX2CopyColorBufferToScanBuffer ──► scan buffer
```

- [Anti-aliasing (MSAA)](../guide/wiiu_term.md#t5-26) targets must be
  [resolved](../guide/wiiu_term.md#t5-27) before they can be sampled.
- Games reuse memory aggressively: one MEM1 range can be a depth buffer in one pass and a
  colour texture in the next, sometimes with a different format. Emulators track "which
  surface was last written at this address" to keep these views consistent.
- Some results return to the CPU (reading a pixel, occlusion queries
  ([Occlusion query](../guide/wiiu_term.md#t5-36)), copies to linear memory). These are the only
  points where the CPU must wait for real GPU work.

## 7.6 Display: scan buffers, flips and vsync

```text
   TV scan buffer ×2 (front/back)        GamePad scan buffer ×2
   ┌────────┐ ┌────────┐                 ┌────────┐ ┌────────┐
   │ front  │ │ back   │◄── game copies  │ front  │ │ back   │
   └────────┘ └────────┘    its frame    └────────┘ └────────┘
        ▲          │
        │   GX2SwapScanBuffers (flip queued)
        │          ▼
   ─────┴──── vertical blank (59.94 Hz) ──── flip happens, flip callback, next frame
```

- [Scan buffers](../guide/wiiu_term.md#t5-28) are what the display controller sends out; the
  game copies its finished frame into the back one.
- [Swap / flip](../guide/wiiu_term.md#t5-29) exchanges front and back at the next
  [vertical blank](../guide/wiiu_term.md#t5-30).
- The [swap interval](../guide/wiiu_term.md#t5-31) sets how many vblanks a frame stays: 1 for
  60 FPS, 2 for 30 FPS (Breath of the Wild).
- `GX2WaitForVsync` / `GX2WaitForFlip` block the game until then; this is what paces a game to
  real time, and what an emulator uses as its frame boundary.

## 7.7 GX2R and invalidation

```text
   GX2RCreateBuffer ─► GX2RLockBuffer ─► CPU writes ─► GX2RUnlockBuffer
                                                            │ implies
                                                            ▼
                                     GX2Invalidate(CPU cache flush + GPU cache invalidate)
```

[GX2R](../guide/wiiu_term.md#t5-35) wraps buffers and surfaces with lock/unlock calls.
[`GX2Invalidate`](../guide/wiiu_term.md#t5-34) is the explicit "this memory changed" signal.
Both tell an emulator when to upload data again, which is far cheaper than hashing every
texture every frame.

## 7.8 How shocora maps this

```text
   gx2 HLE (Rust) ──packets──► wiiu_gpu: PM4 executor (GPU thread)
                                  │ register state, surfaces, shader translation
                                  ▼
                               RenderCommand stream
                                  ▼
                               render thread: wgpu (Vulkan / DX12 / Metal) or OpenGL
```

The [GPU thread](../guide/wiiu_term.md#t11-8) plays the command processor; the
[render thread](../guide/wiiu_term.md#t11-9) talks to the PC graphics API
([wgpu / Vulkan / DirectX 12](../guide/wiiu_term.md#t5-39)). The
[resolution factor](../guide/wiiu_term.md#t11-13) scales render targets on the host only;
the guest still sees its native sizes.
