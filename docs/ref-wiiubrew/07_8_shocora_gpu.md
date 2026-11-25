# 7.8 How shocora Emulates It

[← 7.7 Display, vsync and synchronisation](07_7_display_sync.md) · [7. Latte Graphics](07_0_gpu_latte.md) · next: [8. IOSU and IPC →](08_0_iosu_ipc.md)

Sections 7.1 to 7.7 described the console. This one maps each part onto shocora's code and
threads, so you know which file to open when a game shows a graphics bug.

## 7.8.1 Console part → shocora part

```text
  console                                 shocora                                     file / crate
  ───────                                 ───────                                     ────────────
  Espresso cores running GX2 code    ──►  guest CPU threads (JIT), GX2 as HLE (Rust)  core/cafe_os/src/gx2/
  command buffer in MEM2             ──►  the same: real PM4 words in guest memory    (guest RAM)
  command processor + register file  ──►  GPU thread: PM4 executor + register file    core/gpu/src/command/, regs.rs
  draw engine (fetch, shaders, ROPs) ──►  Latte: register state → host draw descr.    core/gpu/src/latte*.rs
  R600 shader cores                  ──►  shader translator R600 → WGSL               core/gpu/src/shader/
  tiling / AddrLib hardware rules    ──►  software AddrLib + (un)tiling               core/gpu/src/addrlib/, tiling.rs
  GPU caches, MEM1 eDRAM             ──►  host GPU textures and buffers               gui/render/src/latte_renderer/
  display controller                 ──►  render thread presents to the window        present.rs, screens.rs
  GamePad video stream               ──►  GamePad view in the GUI; netplay video      screens.rs, gui/netplay/
```

The rule behind the table: the game must be able to see and touch everything it could touch on
the console (the command buffer, register values it reads back, surfaces in RAM). Everything
the game can't see (caches, HiZ, the real shader cores) is replaced by whatever is fastest on
the host [GPU](../guide/wiiu_term.md#t2-15).

## 7.8.2 Three kinds of threads

```text
  ┌──────────────── emulation (core/system) ───────────────┐
  │ CPU threads: guest cores 0, 1, 2 (JIT code)            │
  │   game calls GX2Draw ─► HLE writes PM4 into guest RAM  │
  │   GX2Flush ─► gpu.submit(words) ──────────────┐        │
  └───────────────────────────────────────────────┼────────┘
                                                  ▼  job queue
  ┌──────────────── GPU thread (core/gpu/src/worker.rs) ─────────────────┐
  │ Engine: parse PM4 (7.2) ─► registers (7.3) ─► Latte (draws.rs …)     │
  │   for each draw: gather shaders, textures, buffers, targets          │
  │   ─► RenderCommand list (Upload, Draw, Clear, CopySurface, Flip …)   │
  │   publishes the retired timestamp ─► wakes DrawDone / WaitTimeStamp  │
  └──────────────────────────────────────────────┬───────────────────────┘
                                                 ▼  render commands
  ┌──────────────── render thread (gui/render/src/latte_renderer) ───────┐
  │ creates wgpu textures, pipelines, buffers; records and submits       │
  │ host GPU work (Vulkan / DirectX 12 / Metal / OpenGL through wgpu);   │
  │ presents the TV and GamePad images; does async readbacks             │
  └──────────────────────────────────────────────────────────────────────┘
```

- The [GPU thread](../guide/wiiu_term.md#t11-8) plays the console GPU: it runs in parallel
  with the CPU, as Cemu's `LatteThread` does. The CPU only waits for it where a console CPU
  waits for its GPU: `GX2DrawDone`, timestamps, or too many pending flips (7.7).
- The [render thread](../guide/wiiu_term.md#t11-9) owns the host graphics device. Keeping it
  separate means a slow host driver call does not stop PM4 decoding.
- With [multi-core mode](../guide/wiiu_term.md#t11-7) on, the three guest cores are three host
  threads as well.

Between the GPU thread and the render thread there is only a list of
[`RenderCommand`](../../core/gpu/src/render.rs)s. That list is API neutral, so the same
emulation drives every host API that [wgpu](../guide/wiiu_term.md#t5-39) supports.

## 7.8.3 A draw, step by step

```text
  PM4 DRAW_INDEX_AUTO arrives on the GPU thread
    │
    ├─1─ read the register file: which shaders, targets, blend, depth, viewport (7.3)
    ├─2─ shaders:   cache lookup by program hash ─► miss: translate R600 → WGSL  (7.4)
    ├─3─ textures:  cache lookup by SurfaceKey ─► miss/changed: untile, upload    (7.5)
    ├─4─ buffers:   vertex, index and uniform data copied into the arena
    ├─5─ targets:   find or create host colour/depth textures                     (7.6)
    ├─6─ pipeline:  shader pair + fixed-function state ─► pipeline key            
    └─7─ emit RenderCommand::Draw { pipeline, bindings, ranges }
                                   │
  render thread: create pipeline if new (pipeline cache), bind, draw on the host GPU
```

The main files: [core/gpu/src/latte.rs](../../core/gpu/src/latte.rs) and its children
[draws.rs](../../core/gpu/src/latte/draws.rs),
[draw_state.rs](../../core/gpu/src/latte/draw_state.rs),
[programs.rs](../../core/gpu/src/latte/programs.rs),
[textures.rs](../../core/gpu/src/latte/textures.rs) and
[targets.rs](../../core/gpu/src/latte/targets.rs); on the host side
[gui/render/src/latte_renderer/draw.rs](../../gui/render/src/latte_renderer/draw.rs) and
[pipelines.rs](../../gui/render/src/latte_renderer/pipelines.rs).

## 7.8.4 Shader translation

```text
  guest program (R600 bytes) ─► cf::parse ─► emit::Translator ─► WGSL source ─► wgpu ─► host shader
  fetch shader ──────────────► fetch::parse ─┘
                                          (vertex attributes pulled from the arena)
```

- Registers stay raw 32-bit values (`vec4<u32>`) in the generated code, because an R600
  instruction decides per use whether a register holds a float or an integer.
- Control flow keeps the hardware's execution-mask stack, so loops and branches behave the
  same per pixel.
- The first time a shader appears, the translation and the host driver's compile cost time: a
  short [stutter](../guide/wiiu_term.md#t1-6). The
  [shader and pipeline caches](../guide/wiiu_term.md#t5-38) keep the results, so each shader
  pays that cost once.

Code: [core/gpu/src/shader/mod.rs](../../core/gpu/src/shader/mod.rs) and
[core/gpu/src/shader/emit.rs](../../core/gpu/src/shader/emit.rs).

## 7.8.5 Texture and target caches

```text
   SurfaceKey = (address, format, width, height, depth, tile mode, swizzle, pitch, mips)
                        │
          ┌─────────────┴──────────────────────────────────────────────┐
          │ host surface table                                         │
          │   key ─► wgpu texture, last content hash, "GPU wrote it"   │
          └─────────────┬──────────────────────────────────────────────┘
       bind as texture  │
          ├── written by a draw/copy on the host?  ─► use the host texture as it is
          ├── CPU hint (flush / GX2Invalidate) or hash differs? ─► untile + upload again
          └── otherwise ─► reuse
```

- The hash covers the **whole** surface. A sampled hash (only some bytes) missed small changes
  and showed old data: sinking feet and a dark ground in BotW, garbled glyphs in Mario Tennis.
- When a texture asks for a different format than the host surface holds (7.6.8), the bits are
  [reinterpreted](../../gui/render/src/latte_renderer/reinterpret.rs), not converted.
- Textures nobody used for a long time are dropped (`RenderCommand::ForgetTexture`) and
  uploaded again from guest memory when needed, which bounds VRAM use.

## 7.8.6 The arena: streaming vertex and uniform data

```text
   guest RAM: vertex / index / uniform data the draw uses
        │ copy (ArenaWrite at offset)
        ▼
   ┌──────────────────────── one big host buffer (the arena) ───────────────────────┐
   │ draw 1 data │ draw 2 data │ draw 3 data │ …                        │ ── wrap ─►│
   └────────────────────────────────────────────────────────────────────────────────┘
   ArenaWrap: start over at 0, after making sure the host GPU no longer reads old bytes
```

Games rewrite vertex and uniform data every frame (skinning, particles, constant buffers). One
ring buffer with append-only writes avoids creating thousands of small host buffers.

## 7.8.7 Present, readback and extras

```text
   RenderCommand::CopyToScanBuffer ─► the frame's image for TV or GamePad
   RenderCommand::Flip             ─► present.rs shows the queued images in the window
                                      ├─ optional: xBRZ upscaling on the GPU    upscale.rs, xbrz.wgsl
                                      ├─ optional: frame generation             framegen.rs
                                      └─ optional: colour grading               color_grade.rs
   RenderCommand::Readback         ─► readback.rs: copy target to a host buffer, map it
                                      asynchronously, write bytes into guest RAM later
```

Readbacks (7.6.9) are asynchronous so the render thread never stalls waiting for the host GPU.
Save states use `RenderCommand::ReadSurfaces` to pull host-only surfaces back into the state
file; depth buffers are left out, since games redraw them every frame.

## 7.8.8 Where to look for a graphics bug

```text
  symptom                                   first place to look
  ───────                                   ───────────────────
  black / missing object                    shader translation (shader/), draw state, depth
  wrong colours, glow, white-out            format reinterpretation, sRGB, targets.rs
  old or flickering texture                 texture hash / CPU write hints (textures.rs)
  stretched "spikes" in geometry            vertex fetch, LINEAR_SPECIAL copies (display.rs)
  falls through floors / water              CPU reads a GPU target: readback (readback.rs)
  game hangs on a wait                      timestamps, interrupts, flips (command.rs)
  only the first frame of a shader hitches  shader / pipeline cache (expected once)
```

Each line above is a bug that was actually found that way; the git history records the fixes.

## Summary

shocora keeps the console's *interface* exact (PM4 in RAM, registers, GX2 structures, timing
of waits and interrupts) and replaces the *implementation* (caches, shader cores, display
hardware) with wgpu objects managed on two host threads.

The whole frame, from CPU to screen and with the CPU side included, is followed in
[11. The Life of a Frame](11_life_of_a_frame.md).

Next: [8. IOSU and IPC →](08_0_iosu_ipc.md)
