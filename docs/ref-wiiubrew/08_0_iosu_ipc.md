# 8. IOSU and IPC

[IOSU](../guide/wiiu_term.md#t10-1) is the operating system of the ARM co-processor
([Starbuck](../guide/wiiu_term.md#t2-22)). It owns every device: storage, USB, network,
Bluetooth, the disc drive, the crypto keys. The PowerPC side cannot touch any of them. It sends
requests to IOSU instead, through [IPC](../guide/wiiu_term.md#t10-2), and waits for the answer.

## 8.1 Two operating systems, one console

```text
   ┌──────────────── PowerPC (Espresso) ────────────────┐       ┌──────── ARM (Starbuck) ────────┐
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

Each IOSU process registers one or more named [devices](../guide/wiiu_term.md#t10-3)
(`/dev/...`). A device is like a Unix character device: you open it, send it numbered
commands, and close it.

## 8.2 The request

```text
   IPC request block (in MEM2, cache-line aligned)
   ┌────────────────────────────────────────────────────────────┐
   │ command   : OPEN / CLOSE / READ / WRITE / SEEK / IOCTL / IOCTLV
   │ result    : filled in by IOSU (≥ 0 ok, < 0 error)          │
   │ handle    : device handle from OPEN                        │
   │ args[5]   : depends on command                             │
   │   IOCTL   : request id, in ptr, in size, out ptr, out size │
   │   IOCTLV  : request id, #in, #out, vector table ptr        │
   └────────────────────────────────────────────────────────────┘

   IOCTLV vector table: [ (ptr, size), (ptr, size), ... ]   ← scatter/gather buffers
```

- `IOS_Ioctl` sends one input buffer and one output buffer.
- `IOS_Ioctlv` sends a list of buffers, used when a command needs several (a path, a struct and
  a data buffer, for example).
- All pointers are converted to physical addresses before the ARM sees them.

## 8.3 The round trip

```text
  PowerPC core 1                     Latte IPC registers            ARM (IOSU)
  ──────────────                     ───────────────────            ──────────
  fill request block
  DCFlushRange(block) ───────────►
  write phys addr ──────────────────► PPC1_MSG
  set "send" bit   ──────────────────► PPC1_CTRL ───── interrupt ──► IOSU kernel routes to the
                                                                    device's message queue
                                                                    device process handles it
                                                                    writes result into block
                    ◄──────────────── ARM_MSG / CTRL ◄── reply ──── IOS_ResourceReply
  IPC interrupt on core 1
  coreinit IPC thread:
    sync call  ─► wake the waiting thread
    async call ─► run callback(result, context) or post to a message queue
```

Synchronous calls block the calling [thread](../guide/wiiu_term.md#t4-10); asynchronous ones
return at once and deliver the result later. Games use async heavily for file loading, so the
completion order and the callback thread matter.

## 8.4 The file system device: FSA

```text
   game                     coreinit FS (client side)            IOSU FSA (/dev/fsa)
   ────                     ─────────────────────────            ───────────────────
   FSAddClient ───────────► FSClient (state in guest memory)
   FSOpenFileAsync ───────► FSCmdBlock queued ─► IOCTL OPENFILE ─► resolve /vol/... path
                                                                   on the mounted volume
                            ◄── result + handle ──────────────────
   callback / message ◄──── FS completion thread
   FSReadFile ────────────► IOCTL READFILE (ptr, size, count) ───► bytes into MEM2
```

```text
   mounted volumes a game sees
   /vol/code          its own executables (read-only)
   /vol/content       its data files (read-only)
   /vol/meta          icon, metadata
   /vol/save          its save data (read/write) ── chapter 10
   /vol/storage_mlc01 internal storage (some system titles, nn_save other-title access)
   /vol/external01    SD card (after mounting)
```

The [file system (FS / FSA)](../guide/wiiu_term.md#t8-10) is the busiest IOSU service by far.
`FS*` is the comfortable API with command blocks and async completion; `FSA*` is the
lower-level synchronous API that talks to `/dev/fsa` by handle.

## 8.5 Other services in one table

```text
  ┌──────────────┬──────────────────────────────┬───────────────────────────────────────┐
  │ device       │ PowerPC library              │ what it answers                       │
  ├──────────────┼──────────────────────────────┼───────────────────────────────────────┤
  │ /dev/mcp     │ coreinit MCP_*               │ installed titles, system version,     │
  │              │                              │ devices, update state                 │
  │ /dev/acp_main│ nn_acp, nn_save              │ save folders, title metadata, play    │
  │              │                              │ time, application box                 │
  │ /dev/act     │ nn_act                       │ user accounts, Mii, persistent ids    │
  │ /dev/fpd     │ nn_fp                        │ friends and presence                  │
  │ /dev/boss    │ nn_boss                      │ SpotPass background downloads         │
  │ /dev/nim     │ nn_nim                       │ update and download queue             │
  │ /dev/socket  │ nsysnet, nlibcurl            │ BSD sockets                           │
  │ /dev/odm     │ (disc state)                 │ optical drive status                  │
  │ /dev/ccr_nfc │ nfc, ntag, nn_nfp            │ GamePad NFC reader (amiibo)           │
  └──────────────┴──────────────────────────────┴───────────────────────────────────────┘
```

See [`nn_act`](../guide/wiiu_term.md#t9-1), [`nn_save`](../guide/wiiu_term.md#t9-2),
[`nn_boss`](../guide/wiiu_term.md#t9-3), [`nn_nfp`](../guide/wiiu_term.md#t9-4) and
[Nintendo Network](../guide/wiiu_term.md#t9-7) for what the services mean to a game.

## 8.6 Where shocora answers

```text
   game ─► HLE library function (Rust)
             │  most calls: answered directly, no IPC packet is ever built
             │     FSOpenFile ─► wiiu_iosu::Fsa::open_file(path)
             │     MCP_TitleList ─► "only the running title is installed"
             │     nn_fp ─► "offline, no friends server"
             │
             └─ raw IOS_Open / IOS_Ioctl from the game itself
                   ─► wiiu_iosu device table (/dev/fsa, /dev/mcp, /dev/odm, ...)
                   ─► answered like Cemu's IOSU; async replies queued as guest callbacks
```

Because the game cannot tell how an answer was produced, shocora skips the IPC transport and
the ARM side entirely ([HLE](../guide/wiiu_term.md#t4-3)), and keeps only what the game
observes: return values, written buffers, callback timing and order. Encrypted content is
decrypted when the title is loaded, so IOSU's crypto is never needed.

## 8.7 Syscalls: the other way into the system

```text
   PowerPC user code ── sc ──► Cafe OS kernel (not IOSU)
   used for: memory mapping, process switches, IPC send, cache maintenance of system areas
```

A [syscall](../guide/wiiu_term.md#t10-4) enters the PowerPC kernel, not IOSU. Games almost never
issue one directly: they call `coreinit`, which does. shocora has no PowerPC kernel, so an
`sc` instruction in game code is reported as unsupported.
