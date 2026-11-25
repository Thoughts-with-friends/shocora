# 7.1 GX2 and the Command Flow

[← 7. Latte Graphics](07_0_gpu_latte.md) · next: [7.2 PM4 packets →](07_2_pm4_packets.md)

[GX2](../guide/wiiu_term.md#t5-1) is a [library](../guide/wiiu_term.md#t4-5) (`gx2.rpl`) that
runs inside the game's process on the [CPU](../guide/wiiu_term.md#t2-1). It does not draw
anything itself. Every GX2 call turns into a few words of GPU commands
([PM4](../guide/wiiu_term.md#t5-3) packets) appended to a
[command buffer](../guide/wiiu_term.md#t5-2) in memory. The GPU reads that buffer later.

## 7.1.1 Three kinds of GX2 call

```text
 ┌───────────────────────┬──────────────────────────────────┬─────────────────────────────────┐
 │ kind                  │ examples                         │ what it produces                │
 ├───────────────────────┼──────────────────────────────────┼─────────────────────────────────┤
 │ "Init" helpers        │ GX2InitTexture, GX2InitColorBuffer│ nothing on the GPU: fill a      │
 │ (pure CPU)            │ GX2InitSampler, GX2InitFetchShader│ struct in guest memory with     │
 │                       │ GX2CalcSurfaceSizeAndAlignment   │ precomputed register values     │
 ├───────────────────────┼──────────────────────────────────┼─────────────────────────────────┤
 │ "Set" state calls     │ GX2SetShaders, GX2SetPixelTexture│ packets that write GPU          │
 │                       │ GX2SetColorBuffer, GX2SetBlend…  │ registers (7.3)                 │
 ├───────────────────────┼──────────────────────────────────┼─────────────────────────────────┤
 │ "Do" calls            │ GX2Draw, GX2DrawIndexedEx,       │ packets that start work: a      │
 │                       │ GX2ClearColor, GX2CopySurface,   │ draw, a clear, a copy, a        │
 │                       │ GX2SwapScanBuffers               │ timestamp, a flip               │
 └───────────────────────┴──────────────────────────────────┴─────────────────────────────────┘
```

The "Init" helpers are why GX2 structs matter: a `GX2Texture` the game initialised once
already contains the GPU register words for that texture. `GX2SetPixelTexture` just copies
them into a packet.

```text
   GX2InitTexture(&tex)            GX2SetPixelTexture(&tex, unit 3)
   ┌─────────────────────────┐     ┌──────────────────────────────────────┐
   │ GX2Texture (in MEM2)    │     │ PM4: SET_RESOURCE                    │
   │  surface: w, h, format… │────►│   slot = pixel-shader texture 3      │
   │  regs[5]: precomputed   │copy │   7 words: base, size, format, mips… │
   │  SQ_TEX_RESOURCE words  │     └──────────────────────────────────────┘
   └─────────────────────────┘
```

## 7.1.2 One draw, start to finish

```text
 game                               command buffer after each call
 ────                               ──────────────────────────────
 GX2SetShaders(fs, vs, ps)    ──►   SET_CONTEXT_REG  SQ_PGM_START_FS / _VS / _PS …
                                    SET_CONTEXT_REG  SPI / SQ setup for inputs, outputs
 GX2SetAttribBuffer(0, …)     ──►   SET_RESOURCE     vertex buffer 0 (address, stride)
 GX2SetVertexUniformReg(…)    ──►   SET_ALU_CONST    vec4 constants for the vertex shader
 GX2SetPixelTexture(&t, 0)    ──►   SET_RESOURCE     texture 0
 GX2SetPixelSampler(&s, 0)    ──►   SET_SAMPLER      filter, wrap, LOD
 GX2SetColorBuffer(&cb, 0)    ──►   SET_CONTEXT_REG  CB_COLOR0_BASE / SIZE / VIEW / INFO
 GX2SetDepthBuffer(&db)       ──►   SET_CONTEXT_REG  DB_DEPTH_BASE / SIZE / INFO
 GX2SetBlendControl(…)        ──►   SET_CONTEXT_REG  CB_BLEND0_CONTROL
 GX2DrawIndexedEx(TRIANGLES,  ──►   SET_CONFIG_REG   VGT_PRIMITIVE_TYPE
     count, U16, indices, …)        INDEX_TYPE / NUM_INSTANCES
                                    DRAW_INDEX_2     index buffer address, count
```

Only the last packet does anything visible; everything before it is
[state](07_3_registers_state.md). A [draw call](../guide/wiiu_term.md#t5-5) uses whatever state
is in the registers at that moment, exactly like OpenGL's state machine.

## 7.1.3 Where the command buffer lives and how it reaches the GPU

```text
            GX2Init(attributes: command buffer pool base + size)
                          │
                          ▼
   ┌───────────────────── command buffer pool (MEM2) ─────────────────────┐
   │ [ chunk A: filled, submitted ][ chunk B: being written ][ free ... ] │
   └───────────────────────────────────────────────▲──────────────────────┘
                                                   │ write pointer
   GX2Flush() or chunk full
        │
        ▼
   kernel: append "INDIRECT_BUFFER(chunk B)" to the GPU ring ─► GPU command processor
                                                                  reads chunk B
```

- `GX2Init` gives GX2 a pool of memory for commands. GX2 fills it chunk by chunk.
- `GX2Flush` submits what has been written so far. GX2 also flushes by itself when a chunk is
  full, and `GX2SwapScanBuffers` / `GX2DrawDone` flush as part of their work.
- Submission goes through the Cafe OS kernel, which owns the real GPU ring buffer. The game
  never sees the ring directly.

## 7.1.4 Display lists: recorded commands

```text
   record once                                    replay every frame
   ───────────                                    ──────────────────
   GX2BeginDisplayList(buf, size)                 GX2CallDisplayList(buf, bytes)
     GX2SetShaders(...)                             └─► INDIRECT_BUFFER(buf, bytes)
     GX2SetPixelTexture(...)                            in the current command buffer
     GX2Draw(...)
   bytes = GX2EndDisplayList(buf)                 GX2DirectCallDisplayList(buf, bytes)
                                                    └─► submit buf straight to the ring
```

A [display list](../guide/wiiu_term.md#t5-4) is a command buffer the game keeps. While recording,
GX2 writes into the game's buffer instead of its own pool. Large games record hundreds per
frame (one per object type or per material) and replay them. Because they are plain memory,
the game can also patch values inside a recorded list before replaying it.

```text
   main command buffer
   ┌────────┬───────────────┬────────┬───────────────┬──────────┐
   │ state  │ CALL list A ──┼─┐      │ CALL list B ──┼─┐  SWAP  │
   └────────┴───────────────┘ │      └───────────────┘ │        │
              ┌───────────────┘                ┌───────┘
              ▼                                ▼
   list A: [state][draw][draw]…    list B: [state][draw]…
           returns to the caller           returns to the caller
```

## 7.1.5 Context state: saving the whole register set

```text
   GX2SetupContextStateEx(&ctxA)      GX2SetContextState(&ctxA)
   ┌────────────────────────────┐     ┌──────────────────────────────────────────┐
   │ GX2ContextState (in MEM2)  │     │ load every shadowed register from ctxA   │
   │  shadow copy of config,    │◄───►│ (one big batch of register loads)        │
   │  context, ALU constants,   │     └──────────────────────────────────────────┘
   │  resources, samplers …     │
   └────────────────────────────┘     GX2SetContextState(NULL) ─► stop shadowing
```

A context state is a snapshot of GPU registers kept in memory. GX2 keeps it up to date as the
game sets state ("shadowing"), and switching contexts reloads the whole set at once. Games use
it to switch between, say, a 3D scene setup and a UI setup without re-issuing every call.
shocora sends its own private packet for this (see [7.2.6](07_2_pm4_packets.md)).

## 7.1.6 The CPU and GPU run apart

```text
 CPU:  ─[build frame N]─────────────[build frame N+1]──────────[build frame N+2]──
 GPU:  ──────────[draw frame N-1]──────────[draw frame N]──────────[draw N+1]─────
                            ▲                       ▲
                   GX2Flush hands work over   no waiting unless the game asks:
                                              GX2DrawDone, GX2WaitTimeStamp,
                                              GX2WaitForFlip, reading GPU results
```

The GPU is usually one frame behind. GX2 offers explicit waits
([`GX2DrawDone`](../guide/wiiu_term.md#t5-33), [timestamps](../guide/wiiu_term.md#t5-32)),
covered in [7.7](07_7_display_sync.md). An emulator can run its GPU part on another host thread
for the same reason, as long as it honours those waits.

## 7.1.7 GX2R: the resource helpers

```text
   GX2RCreateBuffer(flags: vertex | CPU write)        allocate via the game's allocator
   p = GX2RLockBufferEx(buf)                          get a CPU pointer
   …write vertices…
   GX2RUnlockBufferEx(buf)                            ─► DCFlushRange + GX2Invalidate
   GX2RSetAttributeBuffer(buf, 0, stride, offset)     ─► SET_RESOURCE as usual
```

[GX2R](../guide/wiiu_term.md#t5-35) wraps buffers and surfaces with create / lock / unlock calls
and does the cache maintenance for the game. Its unlock calls are a precise "this data changed"
signal ([`GX2Invalidate`](../guide/wiiu_term.md#t5-34)), which an emulator uses to know when to
upload data again.

## 7.1.8 Summary

```text
   Init* = fill structs      Set* = state packets      Draw/Clear/Copy/Swap = work packets
   command buffer ─(GX2Flush)─► ring ─► GPU      display lists = reusable packet blocks
   context state = saved register set            the GPU lags the CPU by about a frame
```
