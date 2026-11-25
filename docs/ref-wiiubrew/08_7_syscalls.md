# 8.7 Syscalls: the Other Way into the System

[← 8.6 Where shocora answers](08_6_shocora_iosu.md) · [8. IOSU and IPC](08_0_iosu_ipc.md) · next: [9. Sound and Input →](09_audio_input.md)

IPC is how the PowerPC reaches the *ARM*. A [syscall](../guide/wiiu_term.md#t10-4) is how user
code on the PowerPC reaches the *PowerPC's own kernel*. The two are easy to mix up, so this
short section puts them side by side.

## 8.7.1 Three levels on the PowerPC

```text
   ┌────────────────────────────────────────────────────────────┐
   │ game code (user mode)                                      │
   │   calls coreinit functions like any other library          │
   ├────────────────────────────────────────────────────────────┤
   │ coreinit (user mode, but trusted system library)           │
   │   most work done here: threads, heaps, FS client, IPC      │
   │   driver; uses `sc` only for what needs the kernel         │
   ├──────────────────── sc instruction ────────────────────────┤
   │ Cafe OS kernel (supervisor mode)                           │
   │   memory mapping, process switches, sending IPC,           │
   │   cache maintenance of system areas, interrupts            │
   └────────────────────────────────────────────────────────────┘
                              │ IPC (8.3)
                              ▼
                     IOSU on the ARM (8.1)
```

- `sc` ("system call") is a PowerPC instruction. It switches the core to supervisor mode and
  jumps to the kernel's syscall handler, with the syscall number in a
  [register](../guide/wiiu_term.md#t2-9).
- [`coreinit`](../guide/wiiu_term.md#t4-9) wraps every syscall a program legitimately needs.
  Even sending an IPC request goes through a syscall, because only the kernel may write the
  IPC registers.

## 8.7.2 Syscall vs IPC

```text
  ┌──────────────────────┬────────────────────────────────┬──────────────────────────────────┐
  │                      │ syscall                        │ IPC                              │
  ├──────────────────────┼────────────────────────────────┼──────────────────────────────────┤
  │ target               │ Cafe OS kernel (same CPU)      │ IOSU (the ARM, another CPU)      │
  │ mechanism            │ `sc` instruction, exception    │ request block + registers +      │
  │                      │                                │ interrupt                        │
  │ typical work         │ map memory, switch process     │ files, accounts, network, NFC    │
  │ cost                 │ a mode switch                  │ a round trip to another CPU      │
  │ who issues it        │ coreinit, almost never games   │ libraries; a few games directly  │
  └──────────────────────┴────────────────────────────────┴──────────────────────────────────┘
```

## 8.7.3 In shocora

```text
   guest code executes `sc`
      ─► CPU returns StepEvent::SystemCall
      ─► emulator stops: "unsupported syscall at 0x…"
```

shocora has no PowerPC kernel: `coreinit` itself is HLE, so every function that would issue a
syscall on the console is already answered in Rust. An `sc` in game code therefore means the
game bypassed coreinit, which is not supported, and the emulator stops with a message instead
of running on in an undefined state, in single-core and multi-core mode alike (see
[core/system/src/emulator/runner.rs](../../core/system/src/emulator/runner.rs) and
[core/system/src/emulator/multi.rs](../../core/system/src/emulator/multi.rs)). Homebrew
that exploits the kernel is the usual source of such code.

## Summary

- Syscall = PowerPC user code to PowerPC kernel; IPC = PowerPC to the ARM.
- Games reach both only through coreinit, which shocora replaces as a whole.

Next: [9. Sound and Input →](09_audio_input.md)
