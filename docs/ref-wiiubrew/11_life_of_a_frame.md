# 11. The Life of a Frame

The previous chapters took the console apart. This one puts it back together by following one
frame of a typical 30 FPS game, such as Breath of the Wild, through every part: CPU threads,
Cafe OS, IOSU, the GPU, sound and the display. The second half shows the same frame inside
shocora.

## 11.1 Two vblanks on the console

At 30 FPS a frame lasts two [vertical blanks](../guide/wiiu_term.md#t5-30), about 33.4 ms.

```text
 time ─────────────────────────────────────────────────────────────────────────────────►
 vblank        │                         vblank │                         vblank │
               ▼                                ▼                                ▼
 core 1  ─[input]─[game logic]─[build cmds: GX2Draw ×1000s]─[GX2SwapScanBuffers]─[wait flip]─
 main               │ OSSignal          │ GX2Flush                               │
 thread             ▼                   ▼                                        │
 core 0  ─[physics jobs]──[animation jobs]──[idle]─────────────────────────────────────────
 core 2  ─[streaming: FSReadFileAsync]──[decompress]──[AI jobs]──[idle]────────────────────
               │                         │
 IOSU    ──────┴─[read file blocks]──────┴─[reply + IPC interrupt]────────────────────────
 GPU7    ─────────────[previous frame: draw]──────[this frame: draw ────────][copy to scan]─
 AX      ─█──█──█──█──█──█──█──█──█──█── every 3 ms: frame callback, mix, out ─────────────
 display ──[shows frame N-1]───────────────────────────────────────────────────[frame N]──
```

Things to notice:

- The CPU builds frame N while the GPU still draws frame N-1. Work overlaps; nothing waits
  unless the game asks it to.
- Core 1's main thread does the game logic and records the GPU commands. Cores 0 and 2 run
  jobs handed out through [message queues](../guide/wiiu_term.md#t4-23) and
  [events](../guide/wiiu_term.md#t4-21).
- File reads go to IOSU and come back as callbacks some milliseconds later.
- Audio has its own 3 ms rhythm, independent of the picture.

## 11.2 Step by step

```text
 ① vblank ──► GX2 vsync interrupt ──► flip of frame N-1 ──► flip callback (game)
 ② main thread wakes from GX2WaitForFlip / OSWaitEvent
 ③ VPADRead / KPADRead: read input
 ④ game logic: signal worker threads (OSSignalEvent, OSSendMessage)
       └─► scheduler: those threads become ready ─► other cores switch to them
 ⑤ workers run; main thread waits on their results (OSWaitEvent / OSJoinThread-like)
 ⑥ main thread records GPU commands into the command buffer
       └─ display lists replayed with INDIRECT_BUFFER
 ⑦ GX2CopyColorBufferToScanBuffer (TV + GamePad), GX2SwapScanBuffers
 ⑧ GX2Flush: the buffer goes to the GPU ring ─► the GPU starts on frame N
 ⑨ main thread blocks until the flip (swap interval 2 ─► two vblanks)
```

## 11.3 Where time can be lost

```text
   ┌───────────────────────────────┬──────────────────────────────────────────────────┐
   │ symptom                       │ usual cause                                      │
   ├───────────────────────────────┼──────────────────────────────────────────────────┤
   │ every frame too slow          │ main thread's guest code too slow (CPU emulation)│
   │ cores idle but FPS low        │ threads wait for each other: wake-up latency,    │
   │                               │ lock contention, missed reschedules              │
   │ periodic hitches              │ first-time work: shader / pipeline compile,      │
   │                               │ JIT translation, texture upload                  │
   │ FPS OK, picture behind        │ GPU emulation queue too deep (latency)           │
   │ audio crackle                 │ audio frames late or bunched after a stall       │
   │ long freeze while loading     │ file callbacks delivered late                    │
   └───────────────────────────────┴──────────────────────────────────────────────────┘
```

See [Stutter](../guide/wiiu_term.md#t1-6), [Contention](../guide/wiiu_term.md#t4-18) and
[Working set](../guide/wiiu_term.md#t11-15).

## 11.4 The same frame inside shocora

```text
                       host threads
 ┌──────────────────────────────────────────────────────────────────────────────────────┐
 │ emulation thread (core 1 + frame loop)                                               │
 │   until the next 60 Hz deadline:                                                     │
 │     run guest code (JIT) ── HLE call? ──► Rust library function under the lock       │
 │   at the deadline: vblank work (flip, vsync callbacks, alarms, AX catch-up)           │
 │ core 0 thread · core 2 thread  ── same loop, same lock for HLE calls                  │
 ├──────────────────────────────────────────────────────────────────────────────────────┤
 │ GPU thread: PM4 executor ── state, surfaces, shader translation ─► RenderCommands     │
 │ render thread: wgpu / OpenGL ── draws, presents TV + GamePad                         │
 │ JIT workers: Cranelift translation of hot blocks                                      │
 │ audio thread (host): plays the mixed ring buffer                                      │
 └──────────────────────────────────────────────────────────────────────────────────────┘
```

```text
   console part              shocora part                                    chapter
   ────────────              ────────────                                    ───────
   Espresso cores            interpreter + Cranelift JIT, one host thread      5
                             per core in multi-core mode
   Cafe OS kernel+libraries  Rust HLE (wiiu_cafe_os), one machine lock          6
   decrementer / alarms      deadline checks + alarm list in HLE               5, 6
   IPC + IOSU                direct Rust answers (wiiu_iosu)                   8
   GPU7 command processor    GPU thread (wiiu_gpu)                             7
   display controller        render thread presenting two screens              7
   DSP + AX                  Rust mixer (wiiu_sound)                           9
   GamePad radio             host input, own HID gyro reader                   9
```

## 11.5 How the frame loop paces itself

```text
   real time ──┬──────────────┬──────────────┬──────────────┬──►
               │ deadline k   │ deadline k+1 │ deadline k+2 │       (16.68 ms apart)
   guest       ├─ run cores ─►├─ run cores ─►├─ run cores ─►│
               │   vblank     │   vblank     │   vblank     │
               └──────────────┴──────────────┴──────────────┘
   finished early ─► sleep until the deadline (real time = guest time)
   finished late  ─► next deadline is already due; more than 4 frames behind ─► re-base
```

Guest [time](../guide/wiiu_term.md#t2-13) only advances as fast as the cores run, so a slow
host makes the game slow down instead of skipping frames
([Real time / full speed](../guide/wiiu_term.md#t1-4)). The [FPS](../guide/wiiu_term.md#t1-5)
counter counts the game's own flips.

## 11.6 Reading this frame in the profiler

shocora's headless runner (`PROF=1`) prints, every 10 seconds of emulation:

```text
   flips                 game frames shown ─────────────────────► FPS
   guest / HLE / idle ms per core      where each core's time went
   waits per core        idle waits, wake-ups that found work, lock wait ms
   core start late       time from the frame start to each core running
   join late             time from the deadline until all cores stopped
   past the deadline     the step (guest code, HLE call by name, idle) that
                         kept a core running after the deadline
```

Read them against the table in 11.3: high idle with low FPS points at waiting between threads,
high join-late points at work that does not stop at the frame boundary, and the
past-the-deadline list names the culprit.
