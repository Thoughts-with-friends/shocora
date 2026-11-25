# 7.7 Display, Vsync and Synchronisation

[← 7.6 Render targets, depth and copies](07_6_targets_copies.md) · [7. Latte Graphics](07_0_gpu_latte.md) · next: [7.8 How shocora emulates it →](07_8_shocora_gpu.md)

A finished image is useless until it reaches a screen, and the CPU and GPU run in parallel, so
each side sometimes has to wait for the other. This section covers both: the path from render
target to TV and GamePad, and the clocks (vsync, flips, timestamps) that keep CPU, GPU and
display in step.

## 7.7.1 Two screens, two scan buffers

```text
                ┌────────────────────────────┐          ┌──────────────────────────┐
  game's own    │ TV scan buffer             │          │ display controller (TV)  │
  render target │ GX2SetTVBuffer(ptr, size,  │ ───────► │ reads it every vblank    │──► HDMI
  ─────────────►│   mode, format, buffering) │          │ 720p / 1080p / 480p      │
     copy       └────────────────────────────┘          └──────────────────────────┘
  (7.6.7)
                ┌────────────────────────────┐          ┌──────────────────────────┐
  ─────────────►│ DRC scan buffer            │ ───────► │ GamePad path: H.264      │──► Wi-Fi
                │ GX2SetDRCBuffer(...)       │          │ encode, radio, decode    │   to the
                └────────────────────────────┘          └──────────────────────────┘   GamePad
```

- A [scan buffer](../guide/wiiu_term.md#t5-28) is the memory the display reads from. The game
  allocates it itself: `GX2CalcTVSize` / `GX2CalcDRCSize` return how many bytes it needs, then
  `GX2SetTVBuffer` / `GX2SetDRCBuffer` hand it to GX2.
- "DRC" is the system's name for the [GamePad](../guide/wiiu_term.md#t2-26) screen
  ([DRC](../guide/wiiu_term.md#t2-27)). Its image is always 854 × 480.
- The [TV](../guide/wiiu_term.md#t2-28) mode (720p, 1080p, 480p) comes from the system
  settings (`GX2GetSystemTVScanMode`), so a game asks rather than chooses.
- Scan buffers are double (or triple) buffered: the display shows one while the next frame is
  copied into another.

These calls are ported in [core/cafe_os/src/gx2/display.rs](../../core/cafe_os/src/gx2/display.rs).

## 7.7.2 One frame's way to the screen

```text
   game (CPU)                              GPU                              display
   ──────────                              ───                              ───────
   draw the frame into its own targets ──► executes draws
   GX2CopyColorBufferToScanBuffer(TV)  ──► copy target → TV scan buffer
   GX2CopyColorBufferToScanBuffer(DRC) ──► copy target → DRC scan buffer
   GX2SwapScanBuffers()                ──► "flip request" queued
   GX2Flush()                          ──► command buffer submitted
                                                         │
                                           next vblank where the swap interval
                                           has passed:  FLIP ─────────────────► new image shown
```

The copy to the scan buffer is separate from drawing because the game usually renders at a
different size or format than the screen, and the copy scales and converts on the way. It also
lets one rendered frame feed both screens.

## 7.7.3 Vertical blank and the swap interval

```text
   time ─►   vblank        vblank        vblank        vblank        vblank
             │  16.7 ms    │             │             │             │
   swap      │             │             │             │             │
   interval 1: flip at every vblank  ─► up to 60 fps
   interval 2: flip at every 2nd     ─► up to 30 fps   ●─────────────●─────────────●
   interval 3: flip at every 3rd     ─► up to 20 fps
```

- The [vertical blank](../guide/wiiu_term.md#t5-30) (vblank) is the moment the display has
  finished one picture and starts the next. The Wii U's standard rate is 59.94 Hz (Cemu's
  `LatteTiming.cpp` notes this, and rounds slightly above 60 Hz on PC to avoid tearing).
- The [swap interval](../guide/wiiu_term.md#t5-31) (`GX2SetSwapInterval`) says how many vblanks
  must pass between two [flips](../guide/wiiu_term.md#t5-29). Most 30 fps games set 2. This is
  how a console game locks its frame rate: not with a timer, but by flipping at most every
  second vblank.
- A game that finishes a frame early waits; one that finishes late misses the vblank and holds
  the old picture for one more vblank (a dropped frame, seen as
  [stutter](../guide/wiiu_term.md#t1-6)).

shocora's vblank handler is `CafeOs::vsync` in
[core/cafe_os/src/gx2/command.rs](../../core/cafe_os/src/gx2/command.rs):

```text
   every vblank:
     vsync_count += 1
     raise the VSYNC event callback, wake threads in GX2WaitForVsync
     if a flip is pending  and  vsync_count − last_flip_vsync ≥ swap interval:
         flips_executed += 1
         raise the FLIP event callback, wake threads in GX2WaitForFlip
```

The swap interval used is the game's, raised to a minimum the user may set (frame limit), or
replaced by a forced value (`effective_swap_interval`). Graphic packs such as FPS++ change the
vblank rate itself through their `vsyncFrequency` setting
([core/system/src/emulator/load/boot_part.rs](../../core/system/src/emulator/load/boot_part.rs)).

## 7.7.4 How the CPU is kept from running ahead

```text
   game thread                                 pending flips
   ───────────                                 ─────────────
   frame 1  GX2SwapScanBuffers ─► 1
   frame 2  GX2SwapScanBuffers ─► 2            the GPU or the display is
   frame 3  GX2SwapScanBuffers ─► 3            behind; the CPU keeps
   …                                           producing frames
   frame 6  GX2SwapScanBuffers ─► 5 already pending ─► the thread blocks
                                               until a vblank executes one flip
```

If nothing stopped it, a fast CPU would queue frames forever and input latency would grow
without limit. `GX2SwapScanBuffers` blocks once too many flips are waiting (shocora:
`MAX_PENDING_FLIPS = 5`). Many games add their own limit: they call `GX2WaitForFlip` or
`GX2WaitForVsync` once per frame, or poll `GX2GetSwapStatus`. Blocking here means a
[Cafe OS thread](06_cafe_os.md) sleeps and the [scheduler](../guide/wiiu_term.md#t4-15) runs
other threads meanwhile.

## 7.7.5 Timestamps: "has the GPU got this far yet?"

```text
   CPU side                                    GPU side
   ────────                                    ────────
   GX2Flush ─► buffer #41 submitted            executing buffer #39
   GX2Flush ─► buffer #42 submitted            executing buffer #40
     │                                              │
     GX2GetLastSubmittedTimeStamp() = 42            │ finishes #40 ─► retired = 40
     GX2GetRetiredTimeStamp()       = 40   ◄────────┘
     GX2WaitTimeStamp(41)  ─► sleeps until retired ≥ 41
     GX2DrawDone()         ─► flush, then wait until retired = last submitted
```

- Each submitted command buffer carries a [timestamp](../guide/wiiu_term.md#t5-32): a counter
  the GPU writes back to memory when it has finished everything before it.
- "Submitted" minus "retired" is how far the GPU is behind the CPU.
- [`GX2DrawDone`](../guide/wiiu_term.md#t5-33) is the heavy hammer: wait for everything. Games
  call it before reusing memory the GPU might still be reading, or before the CPU reads a
  result (7.6.9).
- `GX2SubmitUserTimeStamp` puts a timestamp *inside* a buffer and can ask for an
  [interrupt](../guide/wiiu_term.md#t4-27) when the GPU reaches it. shocora raises that
  interrupt only once the GPU has really executed the commands before it
  ([core/cafe_os/src/gx2/interrupt.rs](../../core/cafe_os/src/gx2/interrupt.rs)); raising it
  at submit time made Mario Tennis hang.

## 7.7.6 Event callbacks

```text
   GX2SetEventCallback(type, function, userData)

   ┌──────────────────────┬────┬────────────────────────────────────────────────┐
   │ GX2CallbackEventType │ id │ raised when                                    │
   ├──────────────────────┼────┼────────────────────────────────────────────────┤
   │ TIMESTAMP_TOP        │ 0  │ a user timestamp reaches the top of the pipe   │
   │ TIMESTAMP_BOTTOM     │ 1  │ a user timestamp has fully retired             │
   │ VSYNC                │ 2  │ every vertical blank                           │
   │ FLIP                 │ 3  │ a flip was executed                            │
   └──────────────────────┴────┴────────────────────────────────────────────────┘
```

The callback runs as an interrupt handler on the core that called `GX2Init`
(`GX2GetMainCoreId`), between the game's own code. It must be short: games use it to count
frames or to signal an [event](../guide/wiiu_term.md#t4-21) that a render thread waits on. In
shocora the callback is queued as a guest call and run on the guest CPU like any other
[callback](../guide/wiiu_term.md#t4-28).

## 7.7.7 Cache coherency: GX2Invalidate

```text
   CPU writes vertices ──► CPU data cache ──(DCFlushRange)──► RAM ──► GPU reads RAM
                                                                     │
                                         GX2Invalidate(VERTEX_BUFFER, ptr, size)
                                         tells the GPU to drop its own cached copy
```

CPU and GPU share RAM but not caches. When the CPU changes data the GPU will read, the game
flushes the CPU [cache](../guide/wiiu_term.md#t2-5) (`DCFlushRange`) and then calls
[`GX2Invalidate`](../guide/wiiu_term.md#t5-34), which on the console puts a cache-sync
packet (`SURFACE_SYNC`, PM4 opcode 0x43, see [7.2](07_2_pm4_packets.md)) in the stream. The opposite direction (GPU wrote, CPU reads) needs
the GPU caches flushed and the CPU cache invalidated.

An emulator has no such caches, but the hints are valuable anyway: they say exactly which
memory the CPU just changed, so the host-side texture or buffer cache knows what to re-upload.

## Summary

```text
   CPU ──GX2Flush──► GPU ──draws, copies──► scan buffer ──flip at vblank──► screen
    ▲                 │                                       │
    └── GX2DrawDone / GX2WaitTimeStamp (retired timestamp) ───┘── GX2WaitForFlip / Vsync
```

The display's vblank is the master clock of a Wii U game. The frame rate, the CPU's lead over
the GPU and the pacing of input all fall out of the swap interval and the waits above, which is
why an emulator's frame timing (7.8, chapter 11) has to imitate the vblank faithfully.

Next: [7.8 How shocora emulates it →](07_8_shocora_gpu.md)
