# 8.1 Two Operating Systems, One Console

[← 8. IOSU and IPC](08_0_iosu_ipc.md) · next: [8.2 The IPC request →](08_2_requests.md)

The Wii U has two processors that run two unrelated operating systems. The game only ever runs
on one of them. This section explains why the second one exists and what lives on it.

## 8.1.1 Who owns what

```text
   ┌───────────────── Espresso (PowerPC, 3 cores) ─────────────────┐
   │  Cafe OS kernel        threads, memory mapping, loader         │   chapter 6
   │  coreinit, gx2, nn_*   libraries the game links against        │
   │  the game (RPX + RPLs)                                         │
   │                                                                │
   │  can touch: its own RAM, the GPU (Latte), the IPC registers    │
   │  cannot touch: storage, USB, network, Bluetooth, disc, keys    │
   └───────────────────────────────┬────────────────────────────────┘
                                   │ IPC only
   ┌───────────────────────────────┴────────────────────────────────┐
   │  Starbuck (ARM) running IOSU                                   │   this chapter
   │  owns: NAND / MLC storage, SD slot, USB, Wi-Fi, Bluetooth,     │
   │        disc drive, GamePad radio, SEEPROM / OTP, crypto engine │
   └────────────────────────────────────────────────────────────────┘
```

- [Espresso](../guide/wiiu_term.md#t2-2) is the [PowerPC](../guide/wiiu_term.md#t2-7) CPU that
  runs the game and [Cafe OS](../guide/wiiu_term.md#t4-2).
- [Starbuck](../guide/wiiu_term.md#t2-22) is a small ARM processor inside the
  [Latte](../guide/wiiu_term.md#t2-16) package. It runs [IOSU](../guide/wiiu_term.md#t10-1)
  ("I/O system, Cafe").
- The storage behind the file system is the [NAND / MLC](../guide/wiiu_term.md#t2-23) and
  [SLC](../guide/wiiu_term.md#t2-24) flash; the console's unique keys sit in
  [SEEPROM / OTP](../guide/wiiu_term.md#t2-25). See [chapter 3](03_hardware.md) for the chips.

## 8.1.2 Why split it this way

```text
   security boundary
   ─────────────────
   game code (untrusted, may be buggy or hacked)  ──X──►  keys, flash, signatures
                                                   │
                         only through narrow, checked requests (IPC)
```

1. **Security.** Every encryption key and every signature check lives on the ARM. A game, even
   a compromised one, can only ask; IOSU decides. Disc and download content is decrypted on the
   ARM side, so the PowerPC never sees a key.
2. **Heritage.** The Wii already had a "Starlet" ARM running IOS for the same jobs. The Wii U
   kept the design and extended it; IOSU is its successor.
3. **Offloading.** File system work, network stacks and USB polling run on their own processor
   and do not take CPU time from the game.

## 8.1.3 Boot order: the ARM starts first

```text
   power on
     │
     ▼
   boot0 (ROM in Starbuck) ─► boot1 (from flash) ─► IOSU kernel + processes (fw.img)
                                                         │
                                                         │ IOS-MCP prepares memory, then
                                                         ▼ starts the PowerPC
                                                   Cafe OS kernel on Espresso
                                                         │
                                                         ▼
                                                   system menu, then the game
```

The PowerPC is the *second* system to boot: IOSU decides what it may run. On a
[vWii](../guide/wiiu_term.md#t2-32) boot, IOSU starts the Wii-mode software instead.

## 8.1.4 IOSU's own processes

IOSU is a small multi-process OS. Each process owns a group of hardware and offers services to
the PowerPC through named [devices](../guide/wiiu_term.md#t10-3). The process names follow
WiiUBrew's IOSU documentation. The device column is for orientation only: the devices that
games actually open, and that Cemu and shocora answer, are listed exactly in
[8.5](08_5_services.md).

```text
  ┌──────────────┬─────────────────────────────┬───────────────────────────────────────────┐
  │ process      │ devices it registers (some) │ job                                       │
  ├──────────────┼─────────────────────────────┼───────────────────────────────────────────┤
  │ IOS-KERNEL   │ (none: the kernel itself)   │ scheduling, memory, message queues, IPC   │
  │ IOS-MCP      │ /dev/mcp                    │ "master control": titles, launching,      │
  │              │                             │ system version, updates                   │
  │ IOS-FS       │ /dev/fsa                    │ file systems on MLC, SLC, SD, USB, disc   │
  │ IOS-CRYPTO   │ /dev/crypto                 │ keys and the AES / SHA engines            │
  │ IOS-USB      │ /dev/usb/...                │ USB host, HID devices                     │
  │ IOS-NET      │ /dev/socket, /dev/net/...   │ Wi-Fi and the TCP/IP stack                │
  │ IOS-PAD      │ /dev/ccr_*, /dev/bt...      │ GamePad radio, Wii remotes (Bluetooth)    │
  │ IOS-ACP      │ /dev/acp_main               │ application control: saves, metadata      │
  │ IOS-FPD      │ /dev/act, /dev/fpd          │ accounts and friends                      │
  │ IOS-NIM-BOSS │ /dev/nim, /dev/boss         │ downloads, SpotPass                       │
  │ IOS-NSEC     │ /dev/nsec                   │ SSL / TLS                                 │
  │ IOS-BSP      │ /dev/bsp                    │ board support: power, LEDs, hardware info │
  └──────────────┴─────────────────────────────┴───────────────────────────────────────────┘
```

## 8.1.5 What a "device" is

```text
   IOSU process (e.g. IOS-FS)
   ┌──────────────────────────────────────────────────────────┐
   │ IOS_RegisterResourceManager("/dev/fsa", myQueue)          │  at startup
   │ loop:                                                     │
   │   msg = IOS_ReceiveMessage(myQueue)       ◄────────────── requests from the PowerPC
   │   switch msg.command:                                     │  (and from other IOSU
   │     OPEN  ─► create a per-client handle                   │   processes)
   │     IOCTL ─► run request #n with the in/out buffers       │
   │     CLOSE ─► drop the handle                              │
   │   IOS_ResourceReply(msg, result)          ──────────────► answer goes back
   └──────────────────────────────────────────────────────────┘
```

A device works like a Unix character device: you open it by name, send numbered commands, and
close it. Every device answers the same seven message types (8.2); what an ioctl number means
is up to each device. The same mechanism is used between IOSU processes, which is why some
devices exist only for internal use.

## Summary

- The game's world ends at the IPC registers. Anything involving storage, network, accounts or
  the GamePad radio is a request to the ARM.
- IOSU is a full OS with processes and message queues, but the PowerPC sees only a list of
  named devices.

Next: [8.2 The IPC request →](08_2_requests.md)
