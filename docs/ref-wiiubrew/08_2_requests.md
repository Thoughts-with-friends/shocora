# 8.2 The IPC Request

[← 8.1 Two operating systems](08_1_two_systems.md) · [8. IOSU and IPC](08_0_iosu_ipc.md) · next: [8.3 The round trip →](08_3_round_trip.md)

Every conversation with IOSU is a series of fixed-size *request blocks* in RAM. The PowerPC
fills one in, hands its address to the ARM, and the ARM writes the result back into the same
block. This section shows the block and the few command types it can carry.

## 8.2.1 The request block

The layout below is Cemu's `IPCCommandBody` (`Cafe/IOSU/iosu_ipc_common.h`). All fields are
32-bit big-endian, like everything the PowerPC writes
([endianness](../guide/wiiu_term.md#t2-8)).

```text
   IPC request block (in MEM2, cache-line aligned)
   ┌────────┬──────────────────┬──────────────────────────────────────────────────┐
   │ offset │ field            │ meaning                                          │
   ├────────┼──────────────────┼──────────────────────────────────────────────────┤
   │ +0x00  │ cmdId            │ OPEN = 1, CLOSE = 2, IOCTL = 6, IOCTLV = 7       │
   │ +0x04  │ result           │ written by IOSU: ≥ 0 success, < 0 error          │
   │ +0x08  │ devHandle        │ handle returned by OPEN (unused for OPEN itself) │
   │ +0x0C  │ (5 words)        │ unnamed in Cemu (ukn0C … ukn20)                  │
   │ … 0x20 │                  │                                                  │
   │ +0x24  │ args[0..5]       │ depend on cmdId, see 8.2.2                       │
   │ +0x38  │ prev_cmdId       │ Cemu notes these may exist only on the PowerPC   │
   │ +0x3C  │ prev_devHandle   │ side: a copy of cmdId / handle for the driver    │
   │ +0x40  │ ppcVirt0, 1      │ the PowerPC's own (virtual) pointers, so the     │
   │ +0x44  │                  │ driver can find its buffers again after reply    │
   └────────┴──────────────────┴──────────────────────────────────────────────────┘
```

The ARM does not use the PowerPC's address translation. Every pointer the ARM reads must be a
[physical address](../guide/wiiu_term.md#t3-4), so the IPC driver converts pointers before
sending and keeps the [effective](../guide/wiiu_term.md#t3-3) (virtual) ones for itself
(`ppcVirt0/1`). Alignment to a [cache line](../guide/wiiu_term.md#t3-8) matters because the
block is flushed out of the CPU cache before the ARM reads it (8.3).

## 8.2.2 The command types and their arguments

```text
  ┌─────────┬──────────────────────────────────────────────────────────────────────────┐
  │ command │ args[0..4]                                                               │
  ├─────────┼──────────────────────────────────────────────────────────────────────────┤
  │ OPEN    │ path ptr, path length + 1, flags          ─► result = new handle (≥ 0)   │
  │ CLOSE   │ (none: only devHandle is set)                                            │
  │ READ    │ buffer, size                ┐ also part of the protocol (ids 3, 4, 5     │
  │ WRITE   │ buffer, size                │ by IOS convention); the devices games use  │
  │ SEEK    │ offset, origin              ┘ work through IOCTL / IOCTLV instead        │
  │ IOCTL   │ request id, in ptr, in size, out ptr, out size                           │
  │ IOCTLV  │ request id, number of in vectors, number of out vectors, vector table    │
  └─────────┴──────────────────────────────────────────────────────────────────────────┘
```

- **OPEN** takes a device path such as `/dev/fsa` and returns a handle, much like `open(2)`.
- **IOCTL** ("I/O control") sends one input buffer and one output buffer. The request id is a
  device-specific command number: for `/dev/odm`, 4 means "check the disc state".
- **IOCTLV** ("vectored") sends any number of buffers, for commands that need several (a path,
  a parameter struct, and a large data buffer, for example).

## 8.2.3 IOCTLV vectors: scatter / gather

```text
   args[3] ──► vector table (array of IPCIoctlVector, 0xC bytes each)
               ┌─────────────┬──────────┬─────────────┐
       [0]     │ baseVirt    │ size     │ basePhys    │ ──► input buffer A    ┐ first numIn
       [1]     │ baseVirt    │ size     │ basePhys    │ ──► input buffer B    ┘ are inputs
       [2]     │ baseVirt    │ size     │ basePhys    │ ──► output buffer C   ┐ the next numOut
       [3]     │ baseVirt    │ size     │ basePhys    │ ──► output buffer D   ┘ are outputs
               └─────────────┴──────────┴─────────────┘
```

Inputs come first, outputs after them. The ARM reads `basePhys`; the PowerPC side uses
`baseVirt` when the reply comes back. "Scatter / gather" means one request covers several
separate memory areas without copying them into one buffer first.

## 8.2.4 Results and error codes

```text
   result ≥ 0   success; for OPEN the value is the handle, for some ioctls a byte count
   result < 0   error. Two families:
                  IOS kernel errors   small negatives, e.g. −4 = IOS_ERROR_INVALID
                                      (unknown device, bad handle, bad arguments)
                  device errors       a device's own codes, e.g. FSA uses −0x30000 − n (8.4)
```

Libraries translate these into their own result types before the game sees them: `FS_STATUS_*`
for the file system, `nn::Result` values for the `nn_*` libraries (8.5).

## 8.2.5 Where the buffers come from: IPCBufPool

```text
   nn_act / nn_acp / … at init:
     IPCBufPoolCreate(memory, size, entrySize, &count, unknown)
     ┌──────────────────────────── memory given by the library ─────────────────────────────┐
     │ IPCBufPool_t header (0x6C) │ ring of entry pointers │ entry │ entry │ entry │ …      │
     │  +0x00 magic 0x0BADF00D    │ (a FIFO: free entries) │                                │
     │  +0x14 entry size          │                        │                                │
     │  +0x1C entry count         │                        │                                │
     └──────────────────────────────────────────────────────────────────────────────────────┘
   per request:
     buf = IPCBufPoolAllocate(pool, size)  ─► fill request struct + vectors ─► IOS_Ioctlv
     IPCBufPoolFree(pool, buf)             ◄─ after the reply
```

The `nn_*` libraries need many small, aligned buffers for request parameters, and must not
allocate from the game's heap at awkward moments. An `IPCBufPool` is a fixed set of equal-size
entries carved out of memory once. shocora lays the header and the entry ring out exactly as
Cemu does, so a title that looks inside finds the same values
([core/cafe_os/src/coreinit/ipc_buf.rs](../../core/cafe_os/src/coreinit/ipc_buf.rs)).

## 8.2.6 A raw request from game code

```text
   handle = IOS_Open("/dev/odm", 0)                    ─► 3   (a handle)
   IOS_Ioctl(handle, 4 /*CHECK_STATE*/, in, 0, out, 4) ─► 0,  *out = 10 (NO_DISC)
   IOS_Close(handle)                                   ─► 0
```

Most games never write code like this: they call `FSOpenFile` or an `nn_*` function, and the
library builds the requests. A few do call `IOS_*` directly, so coreinit exports `IOS_Open`,
`IOS_Close`, `IOS_Ioctl`, `IOS_Ioctlv` and the `*Async` forms
([core/cafe_os/src/coreinit/ios.rs](../../core/cafe_os/src/coreinit/ios.rs)).

## Summary

- One request block, seven message types; IOCTL and IOCTLV carry almost all real work.
- Pointers cross to the ARM as physical addresses; results come back in the same block.
- Errors are negative numbers whose meaning depends on the layer that produced them.

Next: [8.3 The round trip →](08_3_round_trip.md)
