# 8.6 Where shocora Answers

[← 8.5 The other services](08_5_services.md) · [8. IOSU and IPC](08_0_iosu_ipc.md) · next: [8.7 Syscalls →](08_7_syscalls.md)

shocora does not emulate the ARM processor, the IPC registers or IOSU's processes. This section
shows what it does instead, and which observable details it still has to reproduce.

## 8.6.1 Two entry points

```text
   game ─► HLE library function (Rust)
             │  most calls: answered directly, no IPC packet is ever built
             │     FSOpenFile     ─► wiiu_iosu::Fsa::open_file(path)
             │     MCP_TitleList  ─► "only the running title is installed"
             │     nn_fp          ─► "offline, no friends server"
             │
             └─ raw IOS_Open / IOS_Ioctl from the game itself
                   ─► wiiu_iosu device table (/dev/fsa, /dev/mcp, /dev/odm, ...)
                   ─► answered like Cemu's IOSU; async replies queued as guest callbacks
```

1. **Library level** (almost all traffic). The game calls `FSReadFile`, `nn_act::GetMiiName`
   and so on. These are [HLE](../guide/wiiu_term.md#t4-3) functions written in Rust: they read
   the arguments from guest memory and produce the answer at once. No request block is built,
   no physical address computed, no interrupt raised.
2. **Device level** (rare). The game calls `IOS_Open` / `IOS_Ioctl` itself. shocora decodes
   the call and answers from a small device table.

## 8.6.2 What is skipped, what is kept

```text
   console layer                           shocora
   ─────────────                           ───────
   request block, args, vectors            skipped (library level) / read directly (IOS_*)
   physical address translation            skipped
   cache flush / invalidate                no-ops (no caches to keep coherent)
   Latte IPC registers, interrupts         skipped
   IOSU kernel, message queues             skipped
   device process (IOS-FS …)               Rust: core/iosu (Fsa, device table)
   storage, keys, decryption               host folders; content decrypted at load time
   ──────────────────────────────────────────────────────────────────────────────────────
   KEPT EXACTLY:                           return values and error codes
                                           bytes written to the game's buffers
                                           guest-visible structures (FSClient, FSCmdBlock,
                                             IPCBufPool) in guest memory
                                           completion order, callback vs message queue,
                                             "not inside the call" timing
```

Because the game cannot tell how an answer was produced, everything above the line can go.
Everything below it is part of the game's contract and has to match the console (and Cemu,
which matched it first) bit for bit. Encrypted content is decrypted when the title is loaded,
so IOSU's crypto is never needed.

## 8.6.3 The device table

```text
   IOS_Open("/dev/odm")
      │  DeviceKind::from_path: fsa, mcp, acp_main, act, boss, fpd, nim, odm, socket, ccr_nfc
      │                        anything else ─► −4 (IOS_ERROR_INVALID)
      ▼
   Iosu.open_devices: { handle 1 ─► Fsa, handle 2 ─► Odm, … }   (handles count up from 1)

   IOS_Ioctl(handle, request, in, out)
      ├─ unknown handle                         ─► −4
      ├─ Odm + request 4 (CHECK_STATE), out ≥ 4 ─► write 10 (NO_DISC), return 0
      └─ any other open device                  ─► return 0, write nothing

   IOS_Ioctlv(handle, …)                        ─► 0 for an open handle, −4 otherwise
```

Files: [core/iosu/src/lib.rs](../../core/iosu/src/lib.rs) (device kinds, handles) and
[core/cafe_os/src/coreinit/ios.rs](../../core/cafe_os/src/coreinit/ios.rs) (the `IOS_*`
exports). The table is part of the save state, so open handles survive a load.

## 8.6.4 Async replies

```text
   IOS_IoctlAsync(handle, …, callback, user)
      result = same as the sync call
      result ≠ 0 ─► return the error at once, no callback
      result = 0 ─► queue GuestCall { callback, args: [result, user] }  ─► return 0
                         │
                         ▼  later, between guest time slices
                    the callback runs on the guest CPU as a normal guest function
```

The callback is never run inside the HLE call itself. It is queued and executed by the guest
CPU afterwards, like the console's IPC thread would. FS async calls use the same queue for
callbacks, or post an `OSMessage` to the game's [message queue](../guide/wiiu_term.md#t4-23);
see [8.4.7](08_4_fsa.md#847-in-shocora).

## 8.6.5 How this compares to Cemu

```text
   Cemu                                         shocora
   ────                                         ───────
   coreinit builds real IPC request blocks      library calls answered in Rust directly
   IOSU modules run as host threads with        one IOSU object, answers synchronously
   message queues (iosu_fsa, iosu_mcp, …)       from the calling thread
   async results through emulated IPC threads   async results through the guest callback queue
```

Cemu keeps more of the console's structure (request blocks, IOSU threads). shocora keeps only
the observable behaviour, which is less code and no cross-thread hand-off per file read. Cemu's
source is still the reference for *what* each request must answer.

## Summary

- shocora answers at the library level; raw `IOS_*` calls get a minimal device table.
- What must match: results, written bytes, guest-visible structures, completion order.

Next: [8.7 Syscalls: the other way into the system →](08_7_syscalls.md)
