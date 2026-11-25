# 7.6 Render Targets, Depth and Copies

[← 7.5 Surfaces, formats and tiling](07_5_surfaces_tiling.md) · [7. Latte Graphics](07_0_gpu_latte.md) · next: [7.7 Display, vsync and synchronisation →](07_7_display_sync.md)

Section 7.5 described how an image is stored. This section is about the images the GPU
*writes*: [render targets](../guide/wiiu_term.md#t5-17) (colour), the
[depth buffer](../guide/wiiu_term.md#t5-18), and the copy operations that move pixels from one
[surface](../guide/wiiu_term.md#t5-16) to another (MSAA resolve, depth to texture, colour to
the screen).

## 7.6.1 Where the pixels of a draw end up

```text
                      pixel shader (7.4) outputs up to 8 colours per pixel
                                     │
            ┌────────────────────────┼─────────────────────────────┐
            ▼                        ▼                             ▼
   ┌─────────────────┐   ┌─────────────────┐            ┌─────────────────┐
   │ colour target 0 │   │ colour target 1 │    …       │ colour target 7 │   (MRT: CB_COLOR0..7)
   │ GX2ColorBuffer  │   │ GX2ColorBuffer  │            │ GX2ColorBuffer  │
   └─────────────────┘   └─────────────────┘            └─────────────────┘
            ▲  blend + write mask per target (CB_* registers, 7.3)
            │
   depth/stencil test ◄──── ┌──────────────────────────┐
   (before or after the     │ depth buffer             │  one per draw (DB_* registers)
    pixel shader)           │ GX2DepthBuffer + HiZ     │
                            └──────────────────────────┘
```

A draw writes to whatever targets are bound in the [registers](07_3_registers_state.md) at the
moment of the [draw call](../guide/wiiu_term.md#t5-5). Up to eight colour targets can be bound
at once ("multiple render targets", MRT). Deferred renderers such as Breath of the Wild and
Xenoblade Chronicles X use MRT to write a G-buffer (albedo, normals, material data) in one pass.

## 7.6.2 GX2ColorBuffer and GX2DepthBuffer

Both structures start with a full `GX2Surface` (0x74 bytes, see
[7.5.1](07_5_surfaces_tiling.md#751-gx2surface-the-description)) and add a *view* (which mip
and which slices to draw into) and precomputed register values. Offsets below are from Cemu's
`GX2_Surface.h`.

```text
   GX2ColorBuffer (0x9C bytes)                  GX2DepthBuffer (0xAC bytes)
   ┌────────┬──────────────────────────┐        ┌────────┬──────────────────────────────┐
   │ +0x00  │ GX2Surface surface       │        │ +0x00  │ GX2Surface surface           │
   │ +0x74  │ viewMip                  │        │ +0x74  │ viewMip                      │
   │ +0x78  │ viewFirstSlice           │        │ +0x78  │ viewFirstSlice               │
   │ +0x7C  │ viewNumSlices            │        │ +0x7C  │ viewNumSlices                │
   │ +0x80  │ auxData  (MSAA aux/FMASK)│        │ +0x80  │ hiZPtr   (hierarchical Z)    │
   │ +0x84  │ auxSize                  │        │ +0x84  │ hiZSize                      │
   │ +0x88  │ reg: CB_COLOR*_SIZE      │        │ +0x88  │ clearDepth  (float)          │
   │ +0x8C  │ reg: CB_COLOR*_INFO      │        │ +0x8C  │ clearStencil                 │
   │ +0x90  │ reg: CB_COLOR*_VIEW      │        │ +0x90  │ reg: DB_DEPTH_SIZE           │
   │ +0x94  │ reg: CB_COLOR*_MASK      │        │ +0x94  │ reg: DB_DEPTH_VIEW           │
   │ +0x98  │ reg (unknown)            │        │ +0x98  │ reg: DB_DEPTH_BASE           │
   └────────┴──────────────────────────┘        │ +0x9C  │ reg: DB_HTILE_SURFACE        │
                                                │ +0xA0… │ prefetch, preload, poly      │
                                                │        │ offset / DB format control   │
                                                └────────┴──────────────────────────────┘
```

The game fills `surface`, calls [`GX2CalcSurfaceSizeAndAlignment`](07_5_surfaces_tiling.md)
to get sizes, allocates memory, then calls `GX2InitColorBufferRegs` /
`GX2InitDepthBufferRegs`. Those compute the register words once and store them in the
structure, so binding a target later is cheap:

```text
   init time                                        every frame
   ─────────                                        ───────────
   GX2InitColorBufferRegs(&cb)                      GX2SetColorBuffer(&cb, GX2_RENDER_TARGET_0)
     └─ computes CB_COLOR*_SIZE/INFO/VIEW/MASK        └─ SET_CONTEXT_REG CB_COLOR0_BASE = image >> 8
        from surface + view, stores at +0x88…            SET_CONTEXT_REG CB_COLOR0_SIZE/INFO/VIEW
                                                          = the stored words (no recalculation)
```

An emulator must not trust only the stored words: some games build the structure by hand or
patch fields between init and bind. shocora decodes the target from the registers the PM4
stream actually set ([7.3](07_3_registers_state.md)), which is what the GPU would see. The
HLE side of this lives in
[core/cafe_os/src/gx2/render_target.rs](../../core/cafe_os/src/gx2/render_target.rs).

## 7.6.3 Viewport and scissor

```text
   render target 1280 × 720
   ┌──────────────────────────────────────────────────────┐
   │                                                      │
   │   scissor (integer pixels): pixels outside are       │
   │   ┌──────────────────────────────┐   discarded       │
   │   │ viewport (float): maps the   │                   │
   │   │ shader's clip space -1..+1   │                   │
   │   │ onto these pixels, and depth │                   │
   │   │ onto near..far               │                   │
   │   └──────────────────────────────┘                   │
   └──────────────────────────────────────────────────────┘
```

`GX2SetViewport` and `GX2SetScissor` become `PA_CL_VPORT_*` and `PA_SC_*_SCISSOR_*` register
writes. When an emulator renders at a higher
[resolution factor](../guide/wiiu_term.md#t11-13), everything given in pixels (viewport,
scissor, copy rectangles) has to be scaled with the target, or the image covers only a corner.

## 7.6.4 Depth, stencil and HiZ

```text
   for each pixel of a triangle:

        ┌───────────────┐  fail   ┌──────────────┐
        │ HiZ (coarse,  │───────► │ whole 8×8    │   HiZ = "hierarchical Z": a small summary
        │ per tile)     │         │ tile skipped │   buffer at hiZPtr; lets the GPU reject
        └──────┬────────┘         └──────────────┘   big blocks without reading full depth
               │ maybe
               ▼
        ┌───────────────┐  fail   ┌──────────────┐
        │ stencil test  │───────► │ pixel killed │
        └──────┬────────┘         └──────────────┘
               ▼
        ┌───────────────┐  fail   ┌──────────────┐
        │ depth test    │───────► │ pixel killed │   DB_DEPTH_CONTROL: compare function,
        └──────┬────────┘         └──────────────┘   write enable
               ▼
          colour written to the targets, depth written to the depth buffer
```

- Depth formats are listed in [7.5.2](07_5_surfaces_tiling.md#752-formats): `D16`, `D24_S8`,
  `D32F`, `D32F_S8`.
- HiZ and depth compression are internal accelerations. The game only allocates memory for
  them; nothing in the game reads them directly. An emulator can ignore HiZ entirely, and
  shocora does: the host GPU has its own equivalent.
- Depth state (`GX2SetDepthStencilControl`, `GX2SetDepthOnlyControl`) is ported in
  [core/cafe_os/src/gx2/state/depth_stencil_state.rs](../../core/cafe_os/src/gx2/state/depth_stencil_state.rs).

## 7.6.5 Clears

```text
   GX2ClearColor(&cb, r, g, b, a)          GX2ClearDepthStencilEx(&db, depth, stencil, flags)
   GX2ClearBuffersEx(&cb, &db, …)          GX2SetClearDepth / GX2SetClearStencil
        │
        ▼
   console: GX2 draws a full-target rectangle with its own internal shaders
   Cemu   : SubmitHLEClear(...) ─ a special packet the GPU thread turns into a host clear
   shocora: a RenderCommand::Clear for the render thread
```

On the console a clear is just a draw. Emulators replace it with a native clear of the host
texture, which is faster and avoids running GX2's internal shaders. The clear must still be
ordered like a draw: it happens after the draws before it and before the draws after it in the
command buffer.

## 7.6.6 Anti-aliasing and resolve

```text
   MSAA colour buffer (aa = 4)                         single-sample texture
   ┌───────────────────────┐   GX2ResolveAAColorBuffer  ┌─────────────────────┐
   │ each pixel stores 4   │  ───────────────────────►  │ each pixel = average│
   │ samples (+ aux/FMASK) │      ("resolve")           │ of its samples      │
   └───────────────────────┘                            └─────────────────────┘
```

[MSAA](../guide/wiiu_term.md#t5-26) keeps several samples per pixel. Before the image can be
sampled as a texture or shown, it must be [resolved](../guide/wiiu_term.md#t5-27) into a
normal surface. Multi-sample targets are several times larger than normal ones, which matters
when they are meant to fit in the 32 MiB of [MEM1](../guide/wiiu_term.md#t2-20).

## 7.6.7 Surface copies

```text
  ┌───────────────────────────────────┬────────────────────────────────────────────────────┐
  │ call                              │ what it does                                       │
  ├───────────────────────────────────┼────────────────────────────────────────────────────┤
  │ GX2CopySurface(src, mip, slice,   │ copy one mip/slice to another surface; tile mode   │
  │                dst, mip, slice)   │ and format may differ (retile, reinterpret)        │
  │ GX2ResolveAAColorBuffer           │ MSAA → single sample (7.6.6)                       │
  │ GX2ConvertDepthBufferToTexture-   │ depth buffer → a texture the shader can sample     │
  │   Surface                         │ (shadow maps, SSAO, depth of field)                │
  │ GX2CopyColorBufferToScanBuffer    │ finished frame → TV or GamePad scan buffer (7.7)   │
  └───────────────────────────────────┴────────────────────────────────────────────────────┘
```

All four are ported in
[core/cafe_os/src/gx2/display.rs](../../core/cafe_os/src/gx2/display.rs). On the host they
become `RenderCommand::CopySurface` or `RenderCommand::CopyToScanBuffer`
([core/gpu/src/render.rs](../../core/gpu/src/render.rs)), executed by
[gui/render/src/latte_renderer/copy.rs](../../gui/render/src/latte_renderer/copy.rs).

One special case: when either side of `GX2CopySurface` uses the `LINEAR_SPECIAL` tile mode
(see [7.5.3](07_5_surfaces_tiling.md#753-tile-modes)), GX2 does the copy with the CPU in guest
memory, and the game expects to find the bytes there. shocora copies in guest RAM for that case
too; copying only on the host left the vertex data of Breath of the Wild's wind animation stale
and stretched ropes and curtains into spikes.

## 7.6.8 Render to texture: the same bytes, two roles

```text
   frame N, pass 1                       frame N, pass 2
   ───────────────                       ───────────────
   GX2SetColorBuffer(cb @ 0x4000_0000)   GX2SetPixelTexture(tex @ 0x4000_0000)
   draw scene into it                    sample it (bloom, reflections, UI)

            same guest address, same bytes ── written by the GPU, read by the GPU
```

The game never copies anything here: it just points a texture at the memory a target was
drawn into. On the console that works automatically, because both live in the same RAM. An
emulator that keeps targets as host textures has to notice the overlap and either reuse the
host texture directly or reinterpret it if the formats differ (a 32-bit `R11G11B10F` light
buffer read as `R8G8B8A8`, for example). shocora identifies host surfaces by a `SurfaceKey`
(address, format, size, tiling) so a target can be sampled without a round trip through guest
memory ([core/gpu/src/latte/targets.rs](../../core/gpu/src/latte/targets.rs),
[gui/render/src/latte_renderer/reinterpret.rs](../../gui/render/src/latte_renderer/reinterpret.rs)).

## 7.6.9 When the CPU reads what the GPU drew

```text
   GPU draws into a LINEAR_ALIGNED target ─► game's CPU code reads those bytes
   (water height map, collision helper, picking buffer)

   console : the bytes are simply in RAM after GX2DrawDone
   shocora : the render thread reads the target back from the host GPU
             (asynchronously) and writes it into guest RAM
```

Most targets are only ever read by the GPU, so they can stay on the host graphics card. A
target in a *linear* tile mode is a hint that the CPU will read it. shocora reads those back
([gui/render/src/latte_renderer/readback.rs](../../gui/render/src/latte_renderer/readback.rs)).
Without that, Breath of the Wild's Divine Beast Vah Ruta water had no collision and Link fell
through it.

## Summary

- A target is a `GX2Surface` plus a view and precomputed registers; binding it is a handful of
  `SET_CONTEXT_REG` packets.
- Clears, resolves and copies are ordered GPU work, not instant CPU actions.
- Render to texture and CPU readback are where "GPU memory" and "CPU memory" being the same RAM
  on the console costs an emulator the most effort.

Next: [7.7 Display, vsync and synchronisation →](07_7_display_sync.md)
