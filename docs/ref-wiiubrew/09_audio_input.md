# 9. Sound and Input

Both are small compared with the CPU and GPU, but both are timing-sensitive: sound must arrive
every few milliseconds without gaps, and input must be read at the moment the game asks.

## 9.1 AX: the sound engine

```text
   game                         AX (sndcore2)                               out
   ────                         ─────────────                               ───
   AXAcquireVoice ─► voice ──┐
   AXSetVoiceOffsets         │   every audio frame (3 ms, 144 samples @ 48 kHz):
   AXSetVoiceVe (volume)     │
   AXSetVoiceSrc (pitch)     ├─► decode ─► resample ─► volume ─► mix to buses
   AXSetVoiceState(run)      │                                       │
                             │   ┌──────────── main bus ─────────────┤
   up to 96 voices ──────────┘   ├──── aux A ─► effect (reverb) ────┤
                                 ├──── aux B ─► effect (delay) ─────┤
                                 └──── aux C ─► effect ─────────────┤
                                                                     ▼
                                                    final mix ─► TV (up to 6 ch)
                                                              └► GamePad (stereo)
   AXRegisterFrameCallback ◄── called once per audio frame (the game feeds new data here)
```

- A [voice](../guide/wiiu_term.md#t6-2) plays one sound buffer: PCM8, PCM16 or
  [ADPCM](../guide/wiiu_term.md#t6-3), with loop points, pitch, volume ramps and per-output
  mix levels.
- [Aux buses](../guide/wiiu_term.md#t6-4) send part of each voice through effects (reverb,
  chorus, delay) that the game installs as callbacks.
- The [audio frame](../guide/wiiu_term.md#t6-5) is the heartbeat: 3 ms. Every frame the game's
  callback runs and updates voices; streaming music refills its buffers here.
- [DRC audio](../guide/wiiu_term.md#t6-6) is a second mix for the GamePad speakers.

```text
   voice structure in guest memory (simplified)
   ┌────────────────────────────────────────────────────────────┐
   │ state (stop/run) · priority · format                       │
   │ buffer: start, loop, end, current offset (in samples/nibbles)
   │ ADPCM: 16 coefficients, predictor/scale, history            │
   │ SRC: ratio (16.16 fixed) and filter type                    │
   │ mix: per device (TV/DRC) and channel, per bus (main/A/B/C)  │
   │ low-pass / biquad filters                                   │
   └────────────────────────────────────────────────────────────┘
```

Games read voice state back (current offset, "has it stopped?") to drive streaming, so the
offsets must advance exactly as on hardware.

## 9.2 How shocora plays it

```text
   HLE AX ── every 3 ms of guest time ─► mix in Rust (wiiu_sound) ─► ring buffer ─► host audio
                     │
                     └─► queue the game's frame callback on the main core
```

If the game falls behind real time, audio frames must not pile up into a burst (clicks) or be
dropped silently (stutter). shocora paces audio frames by guest time and smooths gaps when the
emulator catches up after a stall.

## 9.3 Input devices

```text
  ┌────────────────────┬──────────────┬──────────────────────────────────────────────┐
  │ device             │ library      │ what the game reads                          │
  ├────────────────────┼──────────────┼──────────────────────────────────────────────┤
  │ GamePad (DRC)      │ vpad         │ buttons, 2 sticks, touch (raw + calibrated), │
  │                    │              │ accelerometer, gyro, magnetometer, volume,   │
  │                    │              │ battery, headphone, mic, camera              │
  │ Wii remote (+ext.) │ padscore     │ buttons, IR pointer, accelerometer,          │
  │   Nunchuk, Classic │ (KPAD/WPAD)  │ extension data, MotionPlus                   │
  │ Pro Controller     │ padscore     │ buttons, 2 sticks (an "extension" type)      │
  └────────────────────┴──────────────┴──────────────────────────────────────────────┘
```

- [VPAD](../guide/wiiu_term.md#t7-1) reads the [GamePad](../guide/wiiu_term.md#t2-26).
  `VPADRead` returns up to 16 buffered samples, newest first; most games take one per frame.
- [KPAD / WPAD](../guide/wiiu_term.md#t7-2) read Bluetooth controllers through `padscore`.
  WPAD is the low level (connection, raw reports); KPAD adds processed values (pointer
  position, stick as floats).
- The [touch panel](../guide/wiiu_term.md#t7-3) reports raw 12-bit coordinates; the game
  calibrates them to the 854×480 screen with `VPADGetTPCalibratedPoint`.
- [Gyro / accelerometer](../guide/wiiu_term.md#t7-4) data come with an orientation matrix
  that VPAD integrates; games such as BotW's shrine puzzles use the matrix directly.

## 9.4 One GamePad sample

```text
   VPADStatus (simplified, one sample)
   ┌──────────────────────────────────────────────────────────────┐
   │ hold / trigger / release  (button bitmasks)                  │
   │ leftStick (x, y) · rightStick (x, y)   floats −1..1          │
   │ acc (x, y, z) · gyro (x, y, z) · angle (x, y, z)              │
   │ dir: 3×3 orientation matrix                                   │
   │ tpData: touch (x, y, touched, validity)                       │
   │ battery · volume · slide volume · headphone · mic status      │
   └──────────────────────────────────────────────────────────────┘
```

`trigger` is "pressed since the last read" and `release` "let go since the last read". They are
computed from consecutive reads, so an emulator must keep the previous state per
controller, not per frame.

## 9.5 Amiibo and NFC

```text
   game ─► nn_nfp (amiibo API) ─► nfc / ntag ─► /dev/ccr_nfc ─► GamePad NFC reader
            mount / get info / read app area / write app area
```

[NFC / amiibo](../guide/wiiu_term.md#t2-31): the figure's data is read through the GamePad.
Without a figure, the detection callbacks report "no tag" after the requested timeout.
