# Wii U Detailed Design Reference

This folder describes how the Wii U is built and how its software fits together, at the level
you need to write or read an emulator. It is the "how the console works" companion to
[`docs/guide/wiiu_term.md`](../guide/wiiu_term.md): when a term is linked, the glossary row
explains it in plain words first.

**Who it is for:** readers at the glossary's *For intermediates* level. You play PC games and
write some code (threads, files, maybe a little DirectX or Vulkan), but you have never looked
inside a console. Register-level detail is kept for the places where an emulator depends on it.

**How to read it:** each chapter opens with one big text diagram, then zooms in. Chapters 3 to
10 each cover one part of the machine. Chapter 11 follows a single frame of a game through all
of them, so read it last, or first if you want the overview before the parts.

## Chapters

|    # | Chapter                                              | What you get                                                          |
| ---: | ---------------------------------------------------- | --------------------------------------------------------------------- |
|    1 | [Emulator Architecture](01_emulator_architecture.md) | How shocora is put together (HLE strategy, boot, frame loop)          |
|    2 | [Workspace Layout](02_workspace_layout.md)           | Which crate does what                                                 |
|    3 | [Hardware Overview](03_hardware.md)                  | The chips, the buses, and where each job happens                      |
|    4 | [Memory Map](04_memory_map.md)                       | Physical and virtual addresses, MEM1 / MEM2, the game's view          |
|    5 | [The Espresso CPU](05_cpu_espresso.md)               | Cores, registers, paired singles, caches, time                        |
|    6 | [Cafe OS](06_cafe_os.md)                             | Kernel, loader, threads, scheduler, sync objects, heaps               |
|    7 | [Latte Graphics (GX2)](07_0_gpu_latte.md)            | From `GX2Draw` to pixels: command buffers, shaders, surfaces, display |
|    8 | [IOSU and IPC](08_0_iosu_ipc.md)                     | The ARM co-processor OS, devices, and how the PowerPC talks to it     |
|    9 | [Sound and Input](09_audio_input.md)                 | AX voices and mixing; GamePad, Wii remotes, Pro Controller            |
|   10 | [Titles, Storage and Saves](10_titles_storage.md)    | Title IDs, folders, the file system, save data                        |
|   11 | [The Life of a Frame](11_life_of_a_frame.md)         | One frame of a game, end to end, and how shocora maps it              |

### Sections of chapters 7 and 8

Chapters 7 and 8 are split into one file per section. Start with the chapter page (7.0 /
8.0), which has the overview diagram and a table of the sections.

- **7. Latte Graphics:** [7.0 overview](07_0_gpu_latte.md) ·
  [7.1 GX2 and the command flow](07_1_gx2_command_flow.md) ·
  [7.2 PM4 packets](07_2_pm4_packets.md) ·
  [7.3 Registers and GPU state](07_3_registers_state.md) ·
  [7.4 Shaders](07_4_shaders.md) ·
  [7.5 Surfaces, formats and tiling](07_5_surfaces_tiling.md) ·
  [7.6 Render targets, depth and copies](07_6_targets_copies.md) ·
  [7.7 Display, vsync and synchronisation](07_7_display_sync.md) ·
  [7.8 How shocora emulates it](07_8_shocora_gpu.md)
- **8. IOSU and IPC:** [8.0 overview](08_0_iosu_ipc.md) ·
  [8.1 Two operating systems](08_1_two_systems.md) ·
  [8.2 The IPC request](08_2_requests.md) ·
  [8.3 The round trip](08_3_round_trip.md) ·
  [8.4 The file system: FS and FSA](08_4_fsa.md) ·
  [8.5 The other services](08_5_services.md) ·
  [8.6 Where shocora answers](08_6_shocora_iosu.md) ·
  [8.7 Syscalls](08_7_syscalls.md)

## Conventions

- Addresses are hexadecimal, written `0x1000_0000` (the underscore only groups digits).
- "Guest" is the Wii U side, "host" is your PC (see [Guest](../guide/wiiu_term.md#t1-2),
  [Host](../guide/wiiu_term.md#t1-3)).
- Boxes marked `[shocora]` show where the emulator replaces a hardware or firmware part.
- Facts about the console follow the community documentation on WiiUBrew and Cemu's source;
  where shocora deliberately differs, the text says so.
