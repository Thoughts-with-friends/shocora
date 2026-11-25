# 7.3 Registers and GPU State

[← 7.2 PM4 packets](07_2_pm4_packets.md) · [7. Latte Graphics](07_0_gpu_latte.md) · next: [7.4 Shaders →](07_4_shaders.md)

Everything the GPU "knows" about the next draw sits in its [registers](../guide/wiiu_term.md#t2-9):
which [shaders](../guide/wiiu_term.md#t2-17) to run, which
[textures](../guide/wiiu_term.md#t5-14) to read, where to write, how to blend. `SET_*` packets
([7.2](07_2_pm4_packets.md)) write them. A draw reads them.

## 7.3.1 The register spaces

```text
   register index (32-bit words)          written by          holds
   ─────────────────────────────          ──────────          ─────
   0x2000 ┌──────────────────────┐
          │ CONFIG               │ ◄─ SET_CONFIG_REG     primitive type, global shader
          │                      │                       setup (SQ_CONFIG), border colours
   0xA000 ├──────────────────────┤
          │ CONTEXT              │ ◄─ SET_CONTEXT_REG    render targets (CB_*), depth (DB_*),
          │                      │                       blend, rasteriser (PA_*), shader
          │                      │                       programs (SQ_PGM_*), interpolation
          │                      │                       (SPI_*), vertex grouping (VGT_*)
   0xC000 ├──────────────────────┤
          │ ALU_CONST            │ ◄─ SET_ALU_CONST      vec4 uniforms: pixel, vertex,
          │                      │                       geometry shader constant files
   0xE000 ├──────────────────────┤
          │ RESOURCE             │ ◄─ SET_RESOURCE       7 words per slot: textures,
          │                      │                       vertex buffers, uniform blocks
   0xF000 ├──────────────────────┤
          │ SAMPLER              │ ◄─ SET_SAMPLER        3 words per slot: filters, wrap
   0xF3FC ├──────────────────────┤
          │ CTL_CONST            │ ◄─ SET_CTL_CONST      base vertex, instance offsets
   0xF880 ├──────────────────────┤
          │ LOOP_CONST           │ ◄─ SET_LOOP_CONST     loop counts for shader loops
   0xF980 ├──────────────────────┤
          │ BOOL_CONST           │ ◄─ SET_BOOL_CONST     booleans for static branches
          └──────────────────────┘
```

The bases come from `core/gpu/src/regs.rs`. Each `SET_*` packet carries an offset relative to
its space's base, so the same packet format serves all spaces.

## 7.3.2 The context registers that matter most

```text
  group  prefix   examples                               decides
  ─────  ──────   ────────                               ───────
  color  CB_      CB_COLOR0_BASE/SIZE/VIEW/INFO          where colour target 0 is, its format,
                  CB_TARGET_MASK, CB_BLEND0_CONTROL      tiling; which channels; blending
  depth  DB_      DB_DEPTH_BASE/SIZE/VIEW/INFO           depth buffer place and format
                  DB_DEPTH_CONTROL, DB_HTILE_DATA_BASE   depth test, stencil, Hi-Z data
  raster PA_      PA_SC_SCREEN_SCISSOR, PA_CL_VPORT_*    scissor, viewport, culling,
                  PA_SU_SC_MODE_CNTL                     polygon offset, face winding
  shader SQ_      SQ_PGM_START_VS/PS/FS/GS               where each shader program is
                  SQ_PGM_RESOURCES_*                     registers it needs, stack size
  interp SPI_     SPI_VS_OUT_CONFIG, SPI_PS_INPUT_CNTL   how vertex outputs reach pixels
  vertex VGT_     VGT_DMA_*, VGT_STRMOUT_*               index fetch, stream-out
```

When you see a bug like "one object is drawn with another's colours", the usual cause is one
of these groups being stale or misread.

## 7.3.3 Slots: resources and samplers per stage

```text
   RESOURCE space (7 words per slot)
   ┌───────────────────────────────┬─────────────────────────────┬──────────────────────────┐
   │ pixel-shader slots            │ vertex-shader slots         │ geometry-shader slots    │
   │  textures t0…                 │  textures, then vertex      │  textures, uniform       │
   │  uniform blocks               │  buffers (fetch shader      │  blocks                  │
   │                               │  reads these), uniform blks │                          │
   └───────────────────────────────┴─────────────────────────────┴──────────────────────────┘

   one texture slot (SQ_TEX_RESOURCE words 0..6), simplified
   w0 dimension, tile mode, pitch       w3 mip base address >> 8
   w1 height, depth, ?                  w4 component swizzle (rgba → which channel)
   w2 base address >> 8                 w5 base/last mip level, base/last array slice
                                        w6 type = texture | vertex buffer
```

A shader asks for "texture 3"; the GPU looks in slot 3 of that stage's range. GX2's
`GX2SetPixelTexture(tex, 3)` simply fills that slot ([7.1.1](07_1_gx2_command_flow.md)).

```text
   pixel shader:  SAMPLE R0, R1.xy, t3, s3
                                   │    │
              RESOURCE slot PS+3 ◄─┘    └─► SAMPLER slot PS+3
              "a 512×512 BC3 texture       "trilinear, wrap U,
               at 0x2034_0000, 10 mips"     clamp V, LOD bias 0"
```

## 7.3.4 Constants: uniform registers and uniform blocks

```text
   mode A: uniform registers                  mode B: uniform blocks
   ─────────────────────────                  ──────────────────────
   SET_ALU_CONST writes vec4 c0…c255          SET_RESOURCE points a slot at a buffer
   shader reads  C12.xyzw directly            in memory; shader fetches from it
   GX2SetVertexUniformReg                     GX2SetVertexUniformBlock
   small, fast to set                         large (bones, light lists), shared
```

A shader is compiled for one of the two modes. [Uniforms](../guide/wiiu_term.md#t5-12) in mode B
are plain memory, so the game can update them with the CPU and only tell the GPU through a cache
invalidate.

## 7.3.5 State is sticky

```text
   SET blend = additive
   DRAW sparks           ─► additive
   DRAW smoke            ─► still additive (nobody changed it)
   SET blend = alpha
   DRAW UI               ─► alpha
```

Registers keep their values until overwritten, across draws and across frames. This is why a
missed `SET_*` packet in an emulator shows up far from where it happened.

## 7.3.6 Context state and register shadowing

```text
   GX2 shadow copy (GX2ContextState in MEM2)        GPU registers
   ┌─────────────────────────────────────┐          ┌─────────────────────┐
   │ config │ context │ ALU │ res │ samp │ ◄─sync──►│ the live values     │
   └─────────────────────────────────────┘          └─────────────────────┘
        GX2SetContextState(&ctx) loads every group at once
```

See [7.1.5](07_1_gx2_command_flow.md#715-context-state-saving-the-whole-register-set). An
emulator must apply the whole saved set, or draws after a context switch use leftovers.

## 7.3.7 How an emulator turns registers into a host draw

```text
   registers at DRAW time
        │
        ├─ SQ_PGM_START_*  ─► shader programs ─► translated shaders   (7.4)
        ├─ RESOURCE/SAMPLER slots ─► textures (untiled, uploaded)      (7.5)
        ├─ CB_* / DB_* ─► render targets in host GPU memory            (7.6)
        ├─ blend, depth, raster ─► host pipeline state
        └─ all of the above, hashed ─► pipeline cache lookup
                                          │
                                          ▼
                              host draw (wgpu / OpenGL)
```

The host GPU needs a [pipeline](../guide/wiiu_term.md#t5-37) object per combination of shaders
and fixed-function state. Building one is slow, so emulators hash the relevant registers and
keep built pipelines in a [shader / pipeline cache](../guide/wiiu_term.md#t5-38). The first time
a combination appears, the build shows up as [stutter](../guide/wiiu_term.md#t1-6).
