# 8.3 The Round Trip

[← 8.2 The IPC request](08_2_requests.md) · [8. IOSU and IPC](08_0_iosu_ipc.md) · next: [8.4 The file system: FS and FSA →](08_4_fsa.md)

Section 8.2 showed what a request looks like. This one follows a request from the PowerPC to
the ARM and back, and explains the two ways a game can wait for the answer.

## 8.3.1 The hardware path

```text
  PowerPC core 1                     Latte IPC registers            ARM (IOSU)
  ──────────────                     ───────────────────            ──────────
  fill request block
  DCFlushRange(block) ───────────►
  write phys addr ──────────────────► PPC1 message register
  set "send" bit   ──────────────────► PPC1 control register ── interrupt ──► IOSU kernel routes
                                                                             it to the device's
                                                                             message queue
                                                                             device process
                                                                             handles it, writes
                                                                             result into block
                    ◄──────────────── ARM message / control ◄── reply ────── IOS_ResourceReply
  IPC interrupt on core 1
  DCInvalidateRange(block)
  coreinit IPC handler:
    sync call  ─► wake the waiting thread
    async call ─► run callback(result, context) or post to a message queue
```

Points worth knowing:

- **Per-core channels.** [Latte](../guide/wiiu_term.md#t2-16) has a separate pair of
  message/control registers for each PowerPC [core](../guide/wiiu_term.md#t2-3). A reply comes
  back as an [interrupt](../guide/wiiu_term.md#t4-27) on the same core that sent the request,
  so every core runs its own IPC driver instance.
- **Cache maintenance.** CPU and ARM share RAM but not the CPU's data
  [cache](../guide/wiiu_term.md#t2-5). The request block and every *input* buffer are flushed
  (`DCFlushRange`) before sending; every *output* buffer is invalidated before the PowerPC reads
  it. Getting this wrong on real hardware gives stale data; an emulator has no cache and can
  treat these calls as hints.
- **One address crosses.** Only the physical address of the block is written to a register.
  Everything else (arguments, vector tables, buffers) is reached through that block.

## 8.3.2 Synchronous calls

```text
   game thread A (core 1)                       other threads
   ──────────────────────                       ─────────────
   FSReadFile(...)  ─► IOS_Ioctlv(...)
                        send request
                        sleep on the reply ─────► scheduler runs thread B, C … on core 1
                                                   (time passes: disc / flash access)
   reply interrupt ─► thread A ready again
   FSReadFile returns the byte count
```

A synchronous call blocks only the calling [thread](../guide/wiiu_term.md#t4-10). The
[scheduler](../guide/wiiu_term.md#t4-15) runs other ready threads on that core while it waits
(see [6. Cafe OS](06_cafe_os.md) for thread states). That is why games put loading on a
separate, lower-[priority](../guide/wiiu_term.md#t4-12) thread: the main thread keeps
rendering while the loader sleeps in IPC.

## 8.3.3 Asynchronous calls

```text
   FSReadFileAsync(client, block, …, &asyncParams)    returns at once (status OK = "queued")
                                         │
            asyncParams ─────────────────┤
            ┌───────────────────────────────────────────────────────┐
            │ userCallback   ─► called as callback(client, block,   │  either a callback …
            │                   result, userContext)                │
            │ userContext                                           │
            │ ioMsgQueue     ─► OSSendMessage(queue, {result …})    │  … or a message
            └───────────────────────────────────────────────────────┘
                                         │
   later: game calls OSReceiveMessage(queue) on its own thread, or the callback runs
```

- With a **callback**, the result is delivered by running a function on a system thread.
  The function must be short and thread-safe; most games just signal an
  [event](../guide/wiiu_term.md#t4-21) or set a flag.
- With a **message queue**, the result is posted as an `OSMessage` to an
  [OSMessageQueue](../guide/wiiu_term.md#t4-23) the game owns. The game picks it up whenever
  it wants, on the thread it wants.
- Raw `IOS_IoctlAsync` / `IOS_IoctlvAsync` take a plain `callback(result, context)` pair.

## 8.3.4 Order and timing matter

```text
   request 1: open  save.dat     ─┐
   request 2: read  save.dat      │  queued on the same FS client:
   request 3: close save.dat     ─┘  executed and completed in this order

   completion  1 ─► 2 ─► 3      the game's state machine expects exactly this
```

Games are written against the real behaviour, including details that look accidental:

- requests on one FS client complete in submission order;
- a callback never runs inside the call that started the request;
- a message queue that is full makes the system thread *wait* until the game frees a slot,
  it does not drop the result.

An emulator that answers immediately must still deliver results in this order and in this
form, or the game's loading state machine stalls. A missing completion is the classic symptom:
the game waits on its message queue forever (a black screen or a frozen save icon).

## 8.3.5 The same picture as a sequence

```text
   game        coreinit FS     IPC driver      Latte regs      IOSU kernel     IOS-FS
    │  FSReadFileAsync  │            │               │               │             │
    │──────────────────►│ queue cmd  │               │               │             │
    │◄── OK (queued) ───│───────────►│ flush, send   │               │             │
    │                   │            │──────────────►│── interrupt ─►│ route       │
    │ (game goes on)    │            │               │               │────────────►│ read
    │                   │            │               │               │◄── reply ───│
    │                   │            │◄── interrupt ─│◄──────────────│             │
    │                   │◄── result ─│               │               │             │
    │◄── callback / message ─────────│               │               │             │
```

## Summary

- The hardware moves one physical address per request and raises one interrupt per reply.
- Sync calls sleep the calling thread; async calls deliver through a callback or a message
  queue later.
- Completion order and delivery rules are part of the contract an emulator must keep.

Next: [8.4 The file system: FS and FSA →](08_4_fsa.md)
