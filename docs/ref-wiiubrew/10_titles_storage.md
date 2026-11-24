# 10. Titles, Storage and Saves

Everything installable on a Wii U is a [title](../guide/wiiu_term.md#t8-1): games, updates,
DLC, system applications, even system libraries. This chapter covers how a title is named,
what is inside it, where it lives, and where its saves go.

## 10.1 Title IDs

```text
   title ID (64 bits) = high 32 bits : low 32 bits

     0005 0000 : 101C 9500      Breath of the Wild (EUR), the game
     0005 000E : 101C 9500      its update
     0005 000C : 101C 9500      its DLC
     ─────────   ─────────
     type        unique per game (same low part ties game, update and DLC together)

   common high parts
     0005_0000  game (application)      0005_0010  system application
     0005_000E  update (patch)          0005_001B  system data
     0005_000C  DLC (add-on content)    0005_0030  system applet
     0005_0002  demo                    0000_0007  vWii
```

The [title ID](../guide/wiiu_term.md#t8-2) is how the system finds a game's
[update](../guide/wiiu_term.md#t8-7) and [DLC](../guide/wiiu_term.md#t8-8): same low half,
different type.

## 10.2 Inside a title

```text
   <title>/
   ├── code/                     ── the executables and launch settings
   │   ├── game.rpx              main program
   │   ├── *.rpl                 the game's own libraries
   │   ├── app.xml               title id, version, SDK version
   │   └── cos.xml               memory sizes, stack size, permissions
   ├── content/                  ── the data: models, textures, sound, scripts
   │   └── ...                   (thousands of files, read through /vol/content)
   └── meta/                     ── what the HOME Menu shows
       ├── meta.xml              names in all languages, product code, flags
       ├── iconTex.tga           icon
       └── bootTvTex.tga, bootDrcTex.tga, bootSound.btsnd
```

- [`code/`](../guide/wiiu_term.md#t8-3), [`content/`](../guide/wiiu_term.md#t8-4) and
  [`meta/`](../guide/wiiu_term.md#t8-5) become `/vol/code`, `/vol/content` and `/vol/meta`.
- [`app.xml` / `cos.xml` / `meta.xml`](../guide/wiiu_term.md#t8-6) are read before the game
  starts: `cos.xml` decides how much MEM2 the process gets and the main thread's stack size.

### Updates overlay the base game

```text
   lookup of /vol/content/Map/MainField.pack
        │
        ├─ in update (0005000E…)?  ── yes ─► use the update's file
        └─ no ─► base game (00050000…)
   code/ comes entirely from the update when one is installed
```

DLC is mounted separately (`/vol/aoc…`) and opened through `nn_aoc`.

## 10.3 Distribution formats

```text
  ┌────────────┬────────────────────────────────────────────────────────────────────┐
  │ format     │ what it is                                                         │
  ├────────────┼────────────────────────────────────────────────────────────────────┤
  │ WUD / WUX  │ raw disc image / the same with duplicate blocks removed            │
  │ NUS        │ eShop download form: title.tmd + title.tik + 0000000N.app (+ .h3)  │
  │ extracted  │ plain code/ content/ meta/ folders (what most emulators run)       │
  │ WUA        │ Cemu's archive: several titles (game + update + DLC) in one file   │
  └────────────┴────────────────────────────────────────────────────────────────────┘
```

See [WUD / WUX](../guide/wiiu_term.md#t8-11), [NUS format](../guide/wiiu_term.md#t8-13) and
[WUA](../guide/wiiu_term.md#t8-12). Disc and NUS content is encrypted with keys held by IOSU;
an emulator decrypts once, at load time.

## 10.4 Internal storage

```text
   mlc01  (internal eMMC, the user-visible storage)
   ├── sys/title/<high>/<low>/        system titles
   ├── usr/title/<high>/<low>/        installed games, updates, DLC
   ├── usr/save/<high>/<low>/         save data, one folder per title
   │     ├── meta/                    save metadata (icon, saveinfo.xml)
   │     └── user/
   │          ├── common/             shared by every account
   │          └── 80000001/           one folder per account (persistent id)
   └── usr/boss/                      SpotPass downloads

   slc01  (small NAND): system files, config, the IOSU and Cafe OS images
```

[`mlc01`](../guide/wiiu_term.md#t8-14) is the folder name Cemu and shocora use for the
emulated internal storage.

## 10.5 Save data

```text
   game                         nn_save                             storage
   ────                         ───────                             ───────
   SAVEInit ─────────────────► mounts /vol/save
   SAVEOpenFile(slot, "a.sav") ─► slot 255 ─► /vol/save/common/a.sav
                                  slot n   ─► /vol/save/<persistent id of account n>/a.sav
                                                 │
                                                 ▼
                                  mlc01/usr/save/00050000/101c9500/user/80000001/a.sav
   SAVEFlushQuota ─────────────► commit (the console journals saves; flush = durable)
```

- [Save data](../guide/wiiu_term.md#t8-9) is per title and per account.
  [`nn_save`](../guide/wiiu_term.md#t9-2) turns a slot number into a folder.
- A sequel can read its predecessor's save (`SAVEOpenFileOtherApplication`), named by title ID
  or by the "unique ID" part.
- Writes are only guaranteed after `SAVEFlushQuota`; games call it after saving. An emulator
  writing straight to host files is always durable, but must still return the same results.

## 10.6 The boot sequence of a title

```text
   1 read meta/, code/app.xml, code/cos.xml
   2 mount /vol/code, /vol/content (+ update overlay), /vol/meta, /vol/save
   3 create the process: MEM2 size from cos.xml, foreground bucket, MEM1
   4 load game.rpx + every RPL it imports (system RPLs ─► HLE in shocora)
   5 run each RPL's entry point (static constructors), then the RPX entry
   6 the game calls coreinit's default setup: heaps, main thread on core 1
   7 the game's main() starts; first GX2Init, first frame ──► chapter 11
```
