# 8.5 The Other Services

[← 8.4 The file system: FS and FSA](08_4_fsa.md) · [8. IOSU and IPC](08_0_iosu_ipc.md) · next: [8.6 Where shocora answers →](08_6_shocora_iosu.md)

Besides the file system, a game talks to IOSU about titles, accounts, friends, downloads, the
network and the GamePad's NFC reader. It almost never opens these devices itself: it calls a
PowerPC library (`MCP_*`, `nn_act`, `nn_fp` …) that builds the requests. This section lists
each service, what a game asks it, and how shocora answers when there is no real console and
no online service behind it.

## 8.5.1 Overview

```text
  ┌──────────────┬──────────────────────────┬─────────────────────────────────────────────┐
  │ device       │ PowerPC library          │ what it answers                             │
  ├──────────────┼──────────────────────────┼─────────────────────────────────────────────┤
  │ /dev/fsa     │ coreinit FS*, FSA*       │ files and directories (8.4)                 │
  │ /dev/mcp     │ coreinit MCP_*           │ installed titles, system version,           │
  │              │                          │ storage devices, update state               │
  │ /dev/acp_main│ nn_acp, nn_save          │ save folders, title metadata, play time,    │
  │              │                          │ parental controls                           │
  │ /dev/act     │ nn_act                   │ user accounts, Mii, persistent ids          │
  │ /dev/fpd     │ nn_fp                    │ friends and presence                        │
  │ /dev/boss    │ nn_boss                  │ SpotPass background downloads               │
  │ /dev/nim     │ nn_nim                   │ update and download queue                   │
  │ /dev/socket  │ nsysnet, nlibcurl        │ BSD sockets                                 │
  │ /dev/odm     │ (disc state)             │ optical drive status                        │
  │ /dev/ccr_nfc │ nfc, ntag, nn_nfp        │ GamePad NFC reader (amiibo)                 │
  └──────────────┴──────────────────────────┴─────────────────────────────────────────────┘
```

These ten are exactly the devices shocora's IOSU accepts in `IOS_Open`
([core/iosu/src/lib.rs](../../core/iosu/src/lib.rs), `DeviceKind::from_path`). Any other path
fails with `IOS_ERROR_INVALID` (−4).

## 8.5.2 MCP: titles and the system

```text
   game asks                               typical use
   ─────────                               ───────────
   MCP_TitleList / MCP_TitleListByAppType  "is my update / DLC installed?"
   MCP_GetSysProdSettings                  region, product area
   MCP_GetTitleInfo                        version of a title
   MCP_GetDeviceId, system version         shown in options menus, sent online
```

MCP ("master control process") knows everything installed on the console. In shocora the only
installed title is the running one, so title lists contain that title when it matches the
filter ([core/cafe_os/src/coreinit/mcp.rs](../../core/cafe_os/src/coreinit/mcp.rs)). See
[title](../guide/wiiu_term.md#t8-1) and [title ID](../guide/wiiu_term.md#t8-2).

## 8.5.3 ACP and nn_save: application control

```text
   nn_save:  SAVEInit, SAVEOpenFile, SAVEMakeDir, SAVEGetSharedDataTitlePath …
             └─ maps "user slot N" to /vol/save/<persistentId>/… and calls FS
   nn_acp:   ACPGetTitleMetaXml, ACPGetApplicationBox, ACPGetNetworkTime, age checks …
```

[`nn_save`](../guide/wiiu_term.md#t9-2) is a thin layer over FS (8.4) that knows where each
user's saves live and creates the folders. `nn_acp` answers metadata questions from
[`meta.xml`](../guide/wiiu_term.md#t8-6) and the save area. In shocora: `nn_save` in
[core/cafe_os/src/coreinit/fs/mutate.rs](../../core/cafe_os/src/coreinit/fs/mutate.rs) and its
children, `nn_acp` in [core/cafe_os/src/nn_acp.rs](../../core/cafe_os/src/nn_acp.rs).
Chapter [10](10_titles_storage.md) explains the save folder layout.

## 8.5.4 ACT: accounts

```text
   console: up to 12 user accounts                 shocora: one offline account
   ┌──────────────────────────────┐                ┌──────────────────────────────┐
   │ slot 1  persistentId 8000000x│                │ slot 1  name, Mii            │
   │         Mii, birthday, region│                │         persistent id        │
   │         Nintendo Network ID? │                │         no network ID        │
   │ slot 2  …                    │                │                              │
   └──────────────────────────────┘                └──────────────────────────────┘
```

[`nn_act`](../guide/wiiu_term.md#t9-1) tells a game who is playing: the account slot, the
*persistent id* (the number that names the user's save folder), the Mii and whether a Nintendo
Network ID is linked. shocora answers as Cemu's IOSU does for an account without a network ID:
not a network account, principal id 0, and online token requests refused with
`NotANetworkAccount` ([core/cafe_os/src/nn/act.rs](../../core/cafe_os/src/nn/act.rs)).

## 8.5.5 FPD: friends and presence

`nn_fp` reports the friend list and sets "now playing" presence. Offline there are no friends:
shocora returns the own account's name and Mii, empty friend lists and `IsOnline` = false.
`LoginAsync` still *succeeds*, because Splatoon soft-locks if it fails; other online requests
fail in their callback ([core/cafe_os/src/nn/fp.rs](../../core/cafe_os/src/nn/fp.rs)).
[Nintendo Network and Miiverse](../guide/wiiu_term.md#t9-7) are gone, so no emulator gets
real answers here.

## 8.5.6 BOSS and NIM: downloads

- [`nn_boss`](../guide/wiiu_term.md#t9-3) ("SpotPass") downloads data in the background, such
  as new stages or news, while the console sleeps.
- `nn_nim` manages system updates and the title download queue. shocora reports no network
  update needed and nothing queued, as Cemu does
  ([core/cafe_os/src/nn_nim.rs](../../core/cafe_os/src/nn_nim.rs)).

## 8.5.7 Sockets and the network

```text
   game ─► nsysnet: socket, connect, send, recv, select …  ─► /dev/socket ─► IOS-NET ─► Wi-Fi
   game ─► nlibcurl / nlibnss (HTTP, TLS)                   ─► the same, plus /dev/nsec for TLS
   game ─► nn_ac: "is the network connected?"
```

The TCP/IP stack runs on the ARM, so even a plain socket is an IPC request. Most games first
ask [`nn_ac`](../guide/wiiu_term.md#t9-6) whether a connection exists and skip online features
if not. shocora answers `nn_ac` (see
[core/cafe_os/src/nn_ac.rs](../../core/cafe_os/src/nn_ac.rs)). It has no `nsysnet`
implementation, so a game's own online features do not work. shocora's netplay is a separate
feature: it streams the GamePad picture and a remote player's input, outside the game.

## 8.5.8 ODM: the disc drive

```text
   IOS_Ioctl(odm, 4 = CHECK_STATE, …) ─► *out = 10 = NO_DISC
```

Games and system libraries ask the optical disc manager whether a disc is inserted. Emulated
games run from installed or extracted content, so shocora answers "no disc", as Cemu does
([core/cafe_os/src/coreinit/ios.rs](../../core/cafe_os/src/coreinit/ios.rs)).

## 8.5.9 NFC: amiibo

```text
   game every frame: NFCProc() / nn_nfp state machine
       Initialize ─► StartDetection ─► (wait for a tag) ─► Mount ─► Read / Write
                                            │
            shocora: no tag is ever touched to the reader ─► timeout ─► "no tag" callback
```

The GamePad has an [NFC reader](../guide/wiiu_term.md#t2-31) for amiibo figures. `nfc` and
`ntag` are the low-level reader and tag format; [`nn_nfp`](../guide/wiiu_term.md#t9-4) is the
amiibo layer on top. shocora keeps Cemu's state machines so games that poll the reader keep
working, but reports that no figure is present
([core/cafe_os/src/nfc.rs](../../core/cafe_os/src/nfc.rs),
[core/cafe_os/src/nn_nfp.rs](../../core/cafe_os/src/nn_nfp.rs)).

## Summary

- Every service is a device behind a PowerPC library; games use the library.
- Offline answers have to be *plausible*, not just "error": a game that cannot handle an error
  (Splatoon's login, an FS miss without the error mask) stalls or shows a fatal screen.

Next: [8.6 Where shocora answers →](08_6_shocora_iosu.md)
