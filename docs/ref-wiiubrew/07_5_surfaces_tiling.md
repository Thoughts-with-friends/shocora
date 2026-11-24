# 7.5 Surfaces, Formats and Tiling

[← 7.4 Shaders](07_4_shaders.md) · [7. Latte Graphics](07_gpu_latte.md) · next: [7.6 Render targets, depth and copies →](07_6_targets_copies.md)

Every image the GPU touches is a [surface](../guide/wiiu_term.md#t5-16): a
[texture](../guide/wiiu_term.md#t5-14), a [render target](../guide/wiiu_term.md#t5-17), a
[depth buffer](../guide/wiiu_term.md#t5-18), a [scan buffer](../guide/wiiu_term.md#t5-28).
This section explains how one is described and how its pixels are arranged in memory.

## 7.5.1 GX2Surface: the description

```text
   GX2Surface (0x74 bytes in guest memory)
   ┌───────────────────────────┬──────────────────────────────────────────────────────┐
   │ dim                       │ 1D, 2D, 3D, CUBE, 2D_ARRAY, 2D_MSAA …                 │
   │ width, height, depth      │ size of mip 0 (depth = slices for 3D / arrays / cube)│
   │ mipLevels                 │ number of mip levels                                 │
   │ format                    │ e.g. R8_G8_B8_A8_UNORM, BC1_UNORM, D24_S8           │
   │ aa                        │ 1, 2, 4 or 8 samples                                 │
   │ use                       │ texture / color buffer / depth buffer / scan buffer  │
   │ imageSize, image ──────────────────────────────────────► mip 0 bytes            │
   │ mipmapSize, mipmaps ───────────────────────────────────► mips 1..n bytes        │
   │ tileMode                  │ how pixels are arranged (7.5.3)                      │
   │ swizzle                   │ bank/pipe swizzle bits (7.5.5)                       │
   │ alignment, pitch          │ required alignment; row length in pixels             │
   │ mipLevelOffset[13]        │ where each mip starts inside `mipmaps`               │
   └───────────────────────────┴──────────────────────────────────────────────────────┘

   GX2CalcSurfaceSizeAndAlignment(&surface) fills: imageSize, mipmapSize, alignment,
   pitch, mipLevelOffset, and may change tileMode (small mips fall back to 1D tiling)
```

The calculation is AMD's [AddrLib](../guide/wiiu_term.md#t5-25). The game uses its results to
allocate memory, so an emulator must produce exactly the same sizes and offsets, or the game
lays out its memory differently from what the GPU part expects (`core/gpu/src/addrlib`).

## 7.5.2 Formats

```text
  ┌──────────────────────┬──────────────┬────────────────────────────────────────────┐
  │ family               │ examples     │ notes                                      │
  ├──────────────────────┼──────────────┼────────────────────────────────────────────┤
  │ plain colour         │ R8G8B8A8,    │ UNORM / SNORM / UINT / SINT / FLOAT /      │
  │                      │ R5G6B5,      │ SRGB variants share one layout             │
  │                      │ R16G16B16A16F│                                            │
  │ packed float         │ R11G11B10F   │ HDR colour in 32 bits                      │
  │ block-compressed     │ BC1 … BC5    │ 4×4 pixel blocks, 8 or 16 bytes each       │
  │ depth / stencil      │ D16, D24_S8, │ also readable as textures with a           │
  │                      │ D32F, D32F_S8│ different "colour" format                  │
  └──────────────────────┴──────────────┴────────────────────────────────────────────┘
```

[Formats](../guide/wiiu_term.md#t5-19) decide bytes per pixel (or per 4×4 block for
[BC1–BC5](../guide/wiiu_term.md#t5-20)). Games often read the same bytes with a different
format than they were written with (an R32 render target sampled as R11G11B10, a depth buffer
sampled as R32F). An emulator has to reinterpret, not convert, in those cases.

## 7.5.3 Tile modes

```text
  ┌──────┬──────────────────────┬────────────────────────────────────────────────────┐
  │ mode │ name                 │ layout                                             │
  ├──────┼──────────────────────┼────────────────────────────────────────────────────┤
  │ 0    │ LINEAR_GENERAL       │ plain rows, no alignment rules                     │
  │ 1    │ LINEAR_ALIGNED       │ plain rows, pitch aligned for the GPU              │
  │ 2    │ 1D_TILED_THIN1       │ 8×8 micro tiles, stored in row order               │
  │ 3    │ 1D_TILED_THICK       │ 8×8×4 micro tiles (volumes)                        │
  │ 4    │ 2D_TILED_THIN1       │ micro tiles grouped in macro tiles over banks and  │
  │ 5–7  │ 2D_TILED_THIN2/4/THICK│ pipes (the common mode for textures and targets)  │
  │ 8–11 │ 2B_TILED_*           │ 2D tiling with bank swapping                       │
  │ 12–15│ 3D_TILED_*           │ for 3D textures                                    │
  │ 16   │ LINEAR_SPECIAL       │ GX2-only: linear, used by copies and readbacks     │
  └──────┴──────────────────────┴────────────────────────────────────────────────────┘
```

## 7.5.4 What tiling does to an image

```text
   a 16×16 image, 32-bit pixels

   LINEAR: memory order follows rows
   row 0 ─────────────────────────────────►  row 1 ──────────────────────────────►
   [p(0,0) p(1,0) … p(15,0)] [p(0,1) … p(15,1)] …

   1D_TILED_THIN1: memory order follows 8×8 tiles
   ┌─────────┬─────────┐       memory:  [ tile A: 64 px = 256 B ][ tile B ][ tile C ][ tile D ]
   │ tile A  │ tile B  │
   ├─────────┼─────────┤       inside a tile, pixels are interleaved (Morton-like order)
   │ tile C  │ tile D  │       so 2×2 neighbours sit in the same 16-32 bytes
   └─────────┴─────────┘

   2D_TILED_THIN1: tiles also spread across memory banks and pipes
   macro tile = several micro tiles; consecutive macro tiles rotate through
   bank 0, bank 1, … and pipe 0, pipe 1 so that the GPU reads them in parallel
```

[Tiling](../guide/wiiu_term.md#t5-21) keeps pixels that are close on screen close in memory,
which is what texture sampling and rasterising need. [Micro tiles](../guide/wiiu_term.md#t5-22)
are always 8×8 pixels; macro tiles depend on the format and on the GPU's bank and pipe count.

## 7.5.5 Pitch, alignment and swizzle

```text
   width 100 px ─► pitch 128 px (rounded up to the tile/alignment rule)
   ┌──────────────── pitch 128 ─────────────────┐
   │ image 100 px                │ padding 28 px│   each row (or row of tiles) is
   └─────────────────────────────┴──────────────┘   pitch wide in memory

   swizzle: extra bank/pipe bits XORed into the address, chosen per surface so that two
   surfaces used together do not start on the same bank
```

- [Pitch](../guide/wiiu_term.md#t5-24) is the stored row length; anything that copies rows
  must use pitch, not width.
- [Swizzle](../guide/wiiu_term.md#t5-23) here means the bank/pipe rotation above (not the
  shader's channel swizzle). Two surfaces with the same address but a different swizzle hold
  pixels in different places.

## 7.5.6 Mipmaps

```text
   image ──► mip 0  (W × H)
   mipmaps ─► mip 1 (W/2 × H/2) at mipLevelOffset[1]
              mip 2 (W/4 × H/4) at mipLevelOffset[2]
              …
              small mips switch from 2D to 1D tiling (and are padded to tile size)
```

[Mipmaps](../guide/wiiu_term.md#t5-15) are stored in a separate allocation after mip 0. Each
level has its own offset, and from some level on its own tile mode, so untiling mip n needs
AddrLib's numbers for that level.

## 7.5.7 The aperture: a linear window onto a tiled surface

```text
   CPU writes linear pixels ──► aperture at 0xE800_0000 + n ──► GPU stores them tiled
                                (GX2AllocateTilingApertureEx)      at the surface's address
```

The [aperture](../guide/wiiu_term.md#t3-9) lets the CPU fill or read a tiled surface as if it
were linear (video playback, screenshots). An emulator translates each access through the
tiling function in software.

## 7.5.8 What the emulator does with all this

```text
   texture bound at draw time
     │  key: address, format, size, tile mode, swizzle, mips, pitch
     ▼
   cache hit and data unchanged? ── yes ─► reuse host texture
     │ no
     ▼
   untile every mip (AddrLib + tiling) ─► decode if the host lacks the format ─► upload
```

Detecting "data unchanged" is the hard part: the GPU may have written the surface (render to
texture), or the CPU may have changed it (streaming). shocora tracks GPU writes per surface
and CPU writes through flush/invalidate hints, and hashes the full contents when unsure
(a sampled hash caused visible corruption; see the project notes).
