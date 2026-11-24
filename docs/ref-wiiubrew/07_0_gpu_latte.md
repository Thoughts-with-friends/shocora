# 7. Latte Graphics (GX2)

Games draw through [GX2](../guide/wiiu_term.md#t5-1), Cafe OS's graphics library, on the
GPU part of [Latte](../guide/wiiu_term.md#t2-16) ("GPU7"). This chapter is split into eight
sections, each in its own file. Read them in order the first time: every section builds on
the one before.

```text
   game ─► GX2 calls ─► PM4 packets ─► GPU registers ─► shaders ─► surfaces ─► screen
            7.1           7.2            7.3              7.4        7.5, 7.6    7.7
                                                                           shocora: 7.8
```

| # | Section | Question it answers |
| ---: | --- | --- |
| 7.1 | [GX2 and the command flow](07_1_gx2_command_flow.md) | What happens between `GX2Draw` and the GPU starting work? |
| 7.2 | [PM4 packets](07_2_pm4_packets.md) | What do the bytes in a command buffer look like? |
| 7.3 | [Registers and GPU state](07_3_registers_state.md) | Where does the GPU keep "the current shader", "the current blend mode"? |
| 7.4 | [Shaders](07_4_shaders.md) | How do the four shader stages work, and what is R600 machine code? |
| 7.5 | [Surfaces, formats and tiling](07_5_surfaces_tiling.md) | How is an image laid out in memory? |
| 7.6 | [Render targets, depth and copies](07_6_targets_copies.md) | Where do pixels go, and how do results move between surfaces? |
| 7.7 | [Display, vsync and synchronisation](07_7_display_sync.md) | How does a frame reach the TV and GamePad, and how do CPU and GPU wait for each other? |
| 7.8 | [How shocora emulates it](07_8_shocora_gpu.md) | Which shocora thread plays which GPU part? |

## The whole picture on one page

```text
 ┌──────────────────────── CPU side (Espresso, game process) ────────────────────────┐
 │                                                                                  │
 │  game code ── GX2SetShaders / GX2SetPixelTexture / GX2SetColorBuffer / GX2Draw    │
 │                   │                                                              │
 │                   ▼  (7.1) GX2 writes packets                                    │
 │  ┌───────────────────────────────────────────┐    ┌───────────────────────────┐  │
 │  │ command buffer (MEM2)                     │    │ display lists (MEM2)      │  │
 │  │ SET_CONTEXT_REG · SET_RESOURCE · DRAW ... │───►│ prerecorded packet blocks │  │
 │  └─────────────────────┬─────────────────────┘    └───────────────────────────┘  │
 │                        │ GX2Flush ─► kernel ─► ring buffer                       │
 └────────────────────────┼─────────────────────────────────────────────────────────┘
                          ▼
 ┌──────────────────────── GPU side (Latte GPU7) ────────────────────────────────────┐
 │  command processor (7.2) ─► register file (7.3) ─► draw engine                   │
 │                                                     │                            │
 │        fetch ─► vertex ─► (geometry) ─► raster ─► pixel shader  (7.4)            │
 │          ▲ textures (7.5)                               │                        │
 │          │                                              ▼                        │
 │    MEM2 / MEM1 surfaces ◄──────────── color + depth targets (7.6)                │
 │                                                         │ copy                   │
 │                                   scan buffers ─► display controller (7.7)       │
 └──────────────────────────────────────────────────────────────────────────────────┘
                                                            │
                                              TV (HDMI) ◄───┴───► GamePad (stream)
```

If you know Direct3D 9/10 or OpenGL, GX2 will feel familiar. The big difference from a PC is
that nothing sits between the game and the GPU: no driver validates or reorders anything, the
game (through GX2) writes the GPU's own command language, and it may read and patch those
commands itself. An emulator therefore has to understand the hardware commands, not just the
API.
