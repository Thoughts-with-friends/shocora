# 6. Cafe OS

[Cafe OS](../guide/wiiu_term.md#t4-2) is the [operating system](../guide/wiiu_term.md#t4-1)
that runs on Espresso next to the game. It has a small kernel and a set of user-mode
[libraries](../guide/wiiu_term.md#t4-5) that the game links against. Almost every library is
"just code" running in the game's own process, which is why an emulator can replace them
function by function ([HLE](../guide/wiiu_term.md#t4-3)).

## 6.1 Layers

```text
  ┌──────────────────────────────── game process ────────────────────────────────┐
  │  game code (RPX)                                                             │
  │     │ calls                                                                  │
  │     ▼                                                                        │
  │  ┌─────────────┬──────────┬──────────┬──────────┬──────────┬─────────────┐   │
  │  │ coreinit    │ gx2      │ sndcore2 │ vpad     │ nn_act   │ … ~100 RPLs │   │
  │  │ threads,    │ graphics │ audio    │ GamePad  │ accounts │ nn_save,    │   │
  │  │ memory, FS, │          │ (AX)     │ input    │          │ proc_ui …   │   │
  │  │ time, IPC   │          │          │          │          │             │   │
  │  └──────┬──────┴────┬─────┴──────────┴──────────┴────┬─────┴─────────────┘   │
  └─────────┼───────────┼────────────────────────────────┼───────────────────────┘
            │ syscalls  │ GPU ring / registers            │ IPC to IOSU (chapter 8)
  ┌─────────▼───────────▼────────────────────────────────▼───────────────────────┐
  │  Cafe OS kernel (PowerPC, supervisor mode): MMU, processes, interrupts, IPC  │
  └──────────────────────────────────────────────────────────────────────────────┘
```

[`coreinit`](../guide/wiiu_term.md#t4-9) is the base library everything else uses. shocora
replaces the library boxes with Rust and has no kernel at all: the few things the
kernel does for a game (memory mapping, interrupts, IPC) are folded into the HLE code.

## 6.2 Processes and the foreground

```text
   ┌──────────────┐  ┌──────────────┐  ┌──────────────┐  ┌──────────────┐
   │ root / system│  │ HOME Menu    │  │ game         │  │ overlays     │
   │ processes    │  │ (background) │  │ (foreground) │  │ (keyboard,   │
   └──────────────┘  └──────────────┘  └──────────────┘  │  error view) │
                                                         └──────────────┘
   foreground switch:  game ──ProcUI "release"──► saves foreground bucket ──► menu runs
                       menu closes ──ProcUI "acquire"──► game resumes
```

Only one application is in the foreground and owns the screens, the
[foreground bucket](../guide/wiiu_term.md#t3-7) and most of the CPU. Games drive this with
[`proc_ui`](../guide/wiiu_term.md#t4-29): every frame they ask "should I keep running, release,
or exit?". An emulator that never leaves the foreground can answer "keep running" forever, but
must deliver the exit request properly when the user closes the game.

## 6.3 Executables: RPX and RPL

```text
   title/code/
   ├─ game.rpx        main executable   ┐ ELF-based, big-endian, sections may be
   ├─ libfoo.rpl      game's own lib    ┘ zlib-compressed
   └─ (system RPLs live on the system, not in the title)

   RPX/RPL sections
   ┌───────────┬───────────┬──────────┬────────────┬────────────┬────────────────┐
   │ .text     │ .rodata   │ .data    │ .bss       │ .fimport_* │ .dimport_*     │
   │ code      │ constants │ globals  │ zeroed     │ functions  │ data imported  │
   │           │           │          │ globals    │ imported   │ from libraries │
   └───────────┴───────────┴──────────┴────────────┴────────────┴────────────────┘
   + .rela.* relocations, export tables, a file-info block (stack size, heap sizes, flags)
```

- An [RPX](../guide/wiiu_term.md#t4-6) is the program; an [RPL](../guide/wiiu_term.md#t4-7) is
  a shared library (like a DLL).
- [Imports and exports](../guide/wiiu_term.md#t4-8) are by name: "coreinit: OSCreateThread".
  The loader fills each import slot with the address of the exported function.
- [`OSDynLoad`](../guide/wiiu_term.md#t4-31) loads more RPLs at run time (like
  `LoadLibrary` + `GetProcAddress`).

```text
   loading in shocora
   game.rpx ─► wiiu_rpl: inflate, place .text in the code area, .data/.bss in MEM2
             ─► apply relocations
             ─► each import "lib:name"
                  ├─ HLE function exists ─► point it at a stub: [hle_call N ; blr]
                  └─ missing              ─► stub that logs "unimplemented" and returns 0
```

## 6.4 Threads

```text
   OSThread (0x6A0 bytes in guest memory, owned by the game)
   ┌─────────────────────────────────────────────────────────────┐
   │ saved registers (r0–r31, f0–f31, CR, LR, CTR, PC …)         │
   │ state: ready / running / waiting / moribund                 │
   │ priority 0 (highest) … 31 (lowest)                          │
   │ affinity mask: which cores may run it (bit 0..2)            │
   │ stack top / bottom, name, TLS, entry point and argument     │
   │ links in the run queue / wait queue                         │
   └─────────────────────────────────────────────────────────────┘
```

- An [OSThread](../guide/wiiu_term.md#t4-11) is a struct the game allocates itself, and
  some games read its fields. So the struct has to stay real, in guest memory, with the
  console's layout.
- [Priority](../guide/wiiu_term.md#t4-12): a ready thread with a higher priority (lower
  number) always runs first on its core.
- [Affinity](../guide/wiiu_term.md#t4-13): pinned threads stay on their core; others may run
  on any allowed core.

## 6.5 The scheduler

```text
         ┌──────────── per core ─────────────┐
         │ run queue (sorted by priority)    │
         │   prio 10: [AudioThread]          │
         │   prio 16: [MainThread] [Loader]  │◄── same priority: take turns
         │   prio 20: [Streamer]             │    every time slice
         └───────────────┬───────────────────┘
                         ▼
                  running thread
                         │ blocks on mutex / queue / sleep
                         ▼
                  wait queue of that object ──(signaled)──► back to a run queue
                                                           + "reschedule" on its core
```

The [scheduler](../guide/wiiu_term.md#t4-15) picks the highest-priority ready thread per core.
A thread runs until it blocks, yields, is preempted by a higher-priority thread becoming ready,
or (for equal priorities) its [time slice](../guide/wiiu_term.md#t4-14) ends. Whenever nothing
is ready, the core idles until an interrupt (alarm, GPU, IPC reply) readies something.

## 6.6 Synchronisation objects

```text
  object              used for                              blocks when
  ──────              ────────                              ───────────
  OSMutex             exclusive section, recursive          owned by another thread
  OSFastMutex         cheap mutex, spins briefly first      contended
  OSCondition         "wait until state changes" + mutex    always (until signaled)
  OSEvent             one-shot or manual-reset flag         not signaled
  OSSemaphore         counted resource                      count is 0
  OSMessageQueue      fixed ring of 16-byte messages        empty (receive) / full (send)
  OSRendezvous        N cores meet at one point             others not arrived
  spinlock            very short sections, no sleeping      (spins instead)
```

All of them are structs in guest memory with a wait queue of threads inside
([Mutex](../guide/wiiu_term.md#t4-17), [Condition variable](../guide/wiiu_term.md#t4-20),
[Event](../guide/wiiu_term.md#t4-21), [Semaphore](../guide/wiiu_term.md#t4-22),
[Message queue](../guide/wiiu_term.md#t4-23), [Rendezvous](../guide/wiiu_term.md#t4-25),
[Spinlock](../guide/wiiu_term.md#t4-19)). When many threads want the same object, that is
[contention](../guide/wiiu_term.md#t4-18), and it is where emulators lose most time.

```text
   thread A (core 1)                       thread B (core 2)
   OSLockMutex(m) ─► owner = A
                                           OSLockMutex(m) ─► owned ─► B waits on m
   ... work ...
   OSUnlockMutex(m) ─► wake B ─► reschedule core 2 ───────► B owns m, runs
```

## 6.7 Alarms, callbacks and interrupts

```text
   OSSetAlarm(alarm, 16.6 ms, handler)
        │
        ▼
   alarm list (sorted by fire time) ─► earliest time ─► decrementer
                                                         │ reaches 0
                                                         ▼
                                       interrupt on that core: run `handler`
                                       in interrupt context (no blocking allowed)
```

An [alarm](../guide/wiiu_term.md#t4-24) runs a guest function at a time. Other
[callbacks](../guide/wiiu_term.md#t4-28) come from the system: vsync and flip callbacks from
GX2, audio frame callbacks from AX, completion callbacks of async file reads. In shocora these
are "guest calls" the HLE side queues and the main core runs on a borrowed stack.

## 6.8 Memory management in the library

```text
   arena (MEM1 / MEM2 / foreground)        heap handles             allocations
   ────────────────────────────────        ────────────             ───────────
   OSGetMemBound(MEM2) ─► MEMCreateExpHeap ─► default heap ─► MEMAllocFromExpHeapEx
                                           ├► frame heap  ─► MEMAllocFromFrmHeapEx
                                           └► unit heap   ─► MEMAllocFromUnitHeap
   MEMAllocFromDefaultHeap is a function POINTER the game may replace with its own allocator
```

Heap headers and free lists live in guest memory, in the console's layout. Some games patch
the default allocator pointers, so the HLE allocator must be reachable through those pointers
rather than called directly.

## 6.9 Other coreinit services in one glance

```text
  time        OSGetTime, OSTicksToCalendarTime, OSSleepTicks
  files       FS* (async with callbacks or message queues) ─► IPC ─► IOSU FSA
  system      OSGetTitleID, OSGetSystemInfo, OSIsDebuggerPresent, OSFatal
  dynamic     OSDynLoad_Acquire / FindExport
  IPC         IOS_Open / IOS_Ioctl(v) / IOS_Close (+ async)
  debugging   OSReport (printf), OSConsoleWrite
  misc        COS / SCI settings, MCP title info, codegen area for JIT-ing games
```

Each one is an HLE function in shocora's `core/cafe_os/src/coreinit/`.
