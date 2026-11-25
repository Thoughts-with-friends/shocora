# 8. IOSU and IPC

[← 7.8 How shocora emulates the GPU](07_8_shocora_gpu.md) · [Contents](README.md) · next: [8.1 Two operating systems →](08_1_two_systems.md)

[IOSU](../guide/wiiu_term.md#t10-1) is the operating system of the ARM co-processor
([Starbuck](../guide/wiiu_term.md#t2-22)). It owns every device: storage, USB, network,
Bluetooth, the disc drive, the crypto keys. The PowerPC side cannot touch any of them. It sends
requests to IOSU instead, through [IPC](../guide/wiiu_term.md#t10-2), and waits for the answer.
This chapter is split into seven sections, each in its own file.

```text
   game ─► library (FS, nn_*) ─► IPC request ─► Latte IPC hardware ─► IOSU device ─► answer
                 8.4, 8.5            8.2              8.3                 8.1          8.3
                                                         shocora skips the middle: 8.6
```

| # | Section | Question it answers |
| ---: | --- | --- |
| 8.1 | [Two operating systems, one console](08_1_two_systems.md) | Why is there a second CPU with its own OS, and what runs on it? |
| 8.2 | [The IPC request](08_2_requests.md) | What does a request look like in memory: open, ioctl, ioctlv? |
| 8.3 | [The round trip](08_3_round_trip.md) | How does a request travel to the ARM and the answer come back, sync or async? |
| 8.4 | [The file system: FS and FSA](08_4_fsa.md) | How does `FSOpenFile` become bytes from a disc or the internal storage? |
| 8.5 | [The other services](08_5_services.md) | What do MCP, ACP, ACT, FPD, BOSS, NIM, sockets, ODM and NFC answer? |
| 8.6 | [Where shocora answers](08_6_shocora_iosu.md) | Which parts does the emulator skip, and what must it still get exactly right? |
| 8.7 | [Syscalls: the other way into the system](08_7_syscalls.md) | How is a syscall different from IPC? |

## The whole picture on one page

```text
   ┌──────────────── PowerPC (Espresso) ─────────────────┐       ┌──────── ARM (Starbuck) ─────────┐
   │ game ─► coreinit FS / nn_act / nn_save / MCP ...    │       │ IOSU kernel                     │
   │                 │                                   │       │  ├ IOS-FS     /dev/fsa          │
   │                 ▼                                   │       │  ├ IOS-MCP    /dev/mcp          │
   │         IPC driver (per core)                       │  IPC  │  ├ IOS-ACP    /dev/acp_main     │
   │         IOS_Open / IOS_Ioctl / IOS_Ioctlv  ─────────┼──────►│  ├ IOS-FPD    /dev/fpd, /dev/act│
   │                                            ◄────────┼───────│  ├ IOS-NET    /dev/socket       │
   │         reply + interrupt                           │       │  ├ IOS-NIM / BOSS / NSEC ...    │
   └─────────────────────────────────────────────────────┘       │  ├ IOS-USB, IOS-PAD, IOS-BSP    │
                                                                 │  └ IOS-CRYPTO (keys never leave)│
                                                                 └─────────────────────────────────┘
```

If you have written programs on Linux, think of IOSU as a separate machine that exposes
character devices (`/dev/...`) over a very small message protocol: open, close, read, write,
seek, ioctl. The PowerPC side has no drivers of its own for any of this hardware.

For an emulator the important point is in 8.6: the game cannot see how an answer was produced,
so shocora does not emulate the ARM at all. It answers the requests directly in Rust, and only
has to match what the game can observe.
