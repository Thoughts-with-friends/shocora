# 7.4 Shaders

[← 7.3 Registers and GPU state](07_3_registers_state.md) · [7. Latte Graphics](07_gpu_latte.md) · next: [7.5 Surfaces, formats and tiling →](07_5_surfaces_tiling.md)

A [shader](../guide/wiiu_term.md#t2-17) is a small program the GPU runs for every vertex or
every pixel. On the Wii U, games ship shaders already compiled to the GPU's machine code, the
[R600 / R700 ISA](../guide/wiiu_term.md#t2-18). There is no HLSL or GLSL source on the disc, so
an emulator must read the machine code and translate it.

## 7.4.1 The four stages

```text
   vertex memory                                                         render targets
        │                                                                      ▲
        ▼                                                                      │
 ┌──────────────┐  ┌──────────────┐  ┌───────────────┐  ┌───────────┐  ┌──────────────┐
 │ fetch shader │─►│ vertex shader│─►│ geometry      │─►│ rasterizer│─►│ pixel shader │
 │ "read and    │  │ "where is    │  │ shader        │  │ (fixed    │  │ "what colour │
 │  unpack the  │  │  this point  │  │ (optional)    │  │  hardware)│  │  is this     │
 │  attributes" │  │  on screen?" │  │ "make or drop │  │ triangles │  │  pixel?"     │
 │              │  │              │  │  primitives"  │  │ → pixels  │  │              │
 └──────────────┘  └──────────────┘  └───────┬───────┘  └───────────┘  └──────────────┘
                                             │ stream-out
                                             ▼
                                        memory buffer
```

- [Fetch shader](../guide/wiiu_term.md#t5-8): reads each [vertex](../guide/wiiu_term.md#t5-7)'s
  attributes from vertex buffers and converts them (for example 16-bit normalised integers to
  floats, with [big-endian](../guide/wiiu_term.md#t2-8) byte swapping). On a PC this is the
  fixed "input layout"; on the Wii U it is a real program, built by
  `GX2InitFetchShaderEx` from the game's attribute list.
- [Vertex shader](../guide/wiiu_term.md#t5-9): computes the screen position and the values
  passed on to pixels (colour, texture coordinates, normals).
- [Geometry shader](../guide/wiiu_term.md#t5-11): optional; runs per
  [primitive](../guide/wiiu_term.md#t5-6), may emit more or fewer of them. On this GPU it runs
  as two programs (the GS itself and a "copy shader" that moves its output on).
- [Pixel shader](../guide/wiiu_term.md#t5-10): computes the colour (and optionally depth) of
  each pixel, sampling [textures](../guide/wiiu_term.md#t5-14).
- [Stream-out](../guide/wiiu_term.md#t5-13): writes vertex or geometry output back to memory
  (used for GPU-side particle and skinning tricks).

## 7.4.2 What a shader looks like in memory

```text
   GX2VertexShader (struct in MEM2, from the game's shader archive .gsh)
   ┌──────────────────────────────────────────────────────────────────┐
   │ regs[]       precomputed SQ_PGM_RESOURCES_VS, SPI_VS_OUT_CONFIG, │
   │              output semantic mapping, …                          │
   │ size, program ──────────────────────────────► machine code (256-│
   │ mode         uniform registers / uniform blocks (7.3.4)         │  byte aligned)
   │ uniform blocks[]  name, slot, size                               │
   │ uniform vars[]    name, type, offset         (for debugging and  │
   │ attrib vars[]     name, type, location        reflection)        │
   │ samplers[]        name, type, slot                               │
   │ stream-out strides, ring sizes (for GS) …                        │
   └──────────────────────────────────────────────────────────────────┘

   GX2SetVertexShader(vs) ─► SET_CONTEXT_REG SQ_PGM_START_VS = program >> 8 (physical)
                             + the regs[] words
```

The game only hands over addresses. The emulator finds the code from `SQ_PGM_START_*`, which
is why shader identity in an emulator is "hash of the machine code plus relevant registers".

## 7.4.3 Inside the machine code: three levels

```text
   CF program (control flow)                      clauses it points to
   ─────────────────────────                      ────────────────────
   00 VTX     → clause @0x40, 2 fetches           ┌ VTX clause: read vertex buffer data
   01 ALU     → clause @0x60, 12 bundles     ────►├ ALU clause: maths, 5 lanes per bundle
   02 TEX     → clause @0x120, 3 samples          ├ TEX clause: texture samples
   03 EXP_DONE POS0  R1                           └ (more clauses …)
   04 EXP_DONE PARAM0 R2   (+ END_OF_PROGRAM)
```

- **CF (control flow)** instructions run in order and do the "big" steps: execute a clause,
  loop, call a subroutine, jump, or **export** results.
- **ALU clauses** do arithmetic.
- **TEX / VTX clauses** fetch from textures or vertex buffers.
- **Exports** are how a shader outputs: `POS` (position), `PARAM` (values for the pixel
  shader), `PIXEL` (colour to render target n), or memory (stream-out).

## 7.4.4 ALU bundles: five operations at once (VLIW5)

```text
   one ALU bundle (issued together)
   ┌────────────┬────────────┬────────────┬────────────┬──────────────────┐
   │ x: MUL     │ y: MUL     │ z: MUL     │ w: MUL     │ t: RECIP_IEEE    │
   │ R0.x=R1.x* │ R0.y=R1.y* │ R0.z=R1.z* │ R0.w=R1.w* │ PS = 1/R2.x      │
   │   C4.x     │   C4.y     │   C4.z     │   C4.w     │ (transcendental) │
   └────────────┴────────────┴────────────┴────────────┴──────────────────┘
   next bundle can read PV.x..w (previous vector result) and PS (previous scalar)
   without going through registers
```

- 128 general registers `R0…R127`, each a vec4. Constants come from `C` (ALU constant file),
  from "kcache" (uniform blocks mapped into the clause), or from literals stored right after
  the bundle.
- `PV` and `PS` forward the previous bundle's results. A translator must model them exactly,
  or values silently come from the wrong place.
- Float behaviour matters: R600 treats `0 × ∞` as 0 in some instructions (`MUL` vs
  `MUL_IEEE`); getting this wrong turns lighting into black or white patches.

## 7.4.5 Texture and vertex fetches

```text
   TEX clause
   SAMPLE      R3, R0.xy__, t0, s0          normal sample
   SAMPLE_L    R4, R0.xyz_, t1, s1, LOD     explicit mip level
   SAMPLE_C    R5, R0.xyzw, t2, s2          compare (shadow maps)
   GATHER4     R6, R0.xy__, t3, s3          the 4 texels around a point
   FETCH4 / LD                              read a texel by integer coordinate

   VTX clause
   FETCH       R1, R0.x, buffer 0, offset 12, format 16_16_FLOAT, swap 8-in-16
```

Each instruction names a resource slot and a sampler slot ([7.3.3](07_3_registers_state.md#733-slots-resources-and-samplers-per-stage)),
plus a destination swizzle (which channels to write, and in which order).

## 7.4.6 Vertex outputs to pixel inputs

```text
   vertex shader exports            SPI registers                pixel shader inputs
   ─────────────────────            ─────────────                ───────────────────
   PARAM0 = texcoord        ──►     semantic 0 → PS input 0 ──►   R0 = interpolated texcoord
   PARAM1 = colour          ──►     semantic 1 → PS input 1 ──►   R1 = interpolated colour
                                    flat / linear / perspective
```

`SPI_VS_OUT_*` and `SPI_PS_INPUT_CNTL_*` connect the two by "semantic" numbers, and say how
to interpolate. A translator must follow these registers, not the export order.

## 7.4.7 Translating to the host

```text
   R600 machine code ─► decode CF / ALU / TEX ─► intermediate form ─► WGSL / GLSL text
                                                                        │
                       host driver compiles it ◄─────────────────────────┘
                                                │
                           pipeline object = shaders + blend/depth/raster state
                                                │
                     shader cache / pipeline cache (memory + disk)
```

- Translation is per unique program, and a host [pipeline](../guide/wiiu_term.md#t5-37) is per
  unique combination of programs and fixed state. Both are cached
  ([shader / pipeline cache](../guide/wiiu_term.md#t5-38)).
- The first sight of a new shader costs translation plus driver compilation:
  [stutter](../guide/wiiu_term.md#t1-6). Emulators hide it with on-disk caches and by building
  pipelines on worker threads.
- [Graphic packs](../guide/wiiu_term.md#t11-12) can replace a shader by its hash with
  hand-written host code (that is how resolution and FPS mods work).

## 7.4.8 Summary

```text
   fetch → vertex → (geometry) → raster → pixel ; stream-out to memory
   machine code = CF program + ALU (VLIW5, PV/PS) + TEX/VTX clauses ; exports out
   emulator: hash code + registers → translate → host pipeline → cache
```
