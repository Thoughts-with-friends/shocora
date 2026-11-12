# shocora

<p align="center">
  <img src="gui/egui/assets/shocora.png" alt="shocora" width="150"/>
</p>

Wii U emulator written in Rust, structured after the hardware and system
software of the console. Its design references [Cemu](https://github.com/cemu-project/Cemu).

## Images

<p align="center">
  <img src="./docs/img/launcher.png" alt="launcher" width="500"/>
</p>
<p align="center">
  <img src="./docs/img/zelda_tw.png" alt="zelda" width="500"/>
</p>
<p align="center">
  <img src="./docs/img/mario_tennis.png" alt="mario 0" width="500"/>
</p>

## Quick Start

- Build

```text
cargo build --release
```

- Utils

```text
cargo xtask run [--graphics wgpu|vulkan|opengl] [-- TITLE_DIR | FILE.rpx]
cargo xtask test
cargo xtask icon   # gui/egui/assets/shocora.svg -> shocora.png, shocora.ico
```

## Features & Configuration Guide

### 🎨 Graphics & Display

- **Graphics Backends**:
  - `wgpu` (default): Automatically selects the best backend (DX12 / Metal / Vulkan / GL).
  - `vulkan`: Forces wgpu's Vulkan backend.
  - `opengl`: Uses native OpenGL through `glow`.
- **3D Resolution**: Scalable from **2x to 8x** (up to 10,240 pixels wide).
  - *Examples*: 8x at 720p, 5x at 1080p.
  - *Note*: Significantly increases VRAM usage.
- **Screen Upscaling**: Runs xBRZ on the GPU at **2x to 6x**.
  - Included under the default `xbrz` feature of `shocora_render` (GPL-3.0 licensed).
- **Frame Rate Control**:
  - **Force 60 fps**: Overrides the title's native swap interval.
    - *Warning*: Titles logic-bound to 30 fps (e.g., *Twilight Princess HD*) will run up to double speed.
  - **Default**: Preserves the target frame rate requested by the title.
  - **Max 30 fps**: Caps titles that run excessively fast while leaving slower ones untouched.

---

### 🎮 Controls & Input

#### Default Keybindings

| Action / Button     | Keyboard Key                 |
| :------------------ | :--------------------------- |
| **A / B / X / Y**   | `X` / `Z` / `S` / `A`        |
| **L / R / ZL / ZR** | `Q` / `W` / `1` / `2`        |
| **+ / −**           | `Enter` / `Backspace`        |
| **D-Pad**           | Arrow Keys (`↑` `↓` `←` `→`) |
| **Left Stick**      | `I` `J` `K` `L`              |

#### Controller & Hotkeys

- **Gamepad Support**: Powered by [gilrs](https://gitlab.com/gilrs-project/gilrs) with plug-and-play functionality.
  - Mapped by physical button position (`East` corresponds to the Wii U's **A** button).
  - Custom mappings can be configured under `[pad_bindings]` in `shocora.toml`.
- **Game Speed Toggle**: Cycle through speeds (`1x` → `2x` → `3x` → `4x` → `0.5x`) using `Tab`, pressing the Left Stick, or clicking the speed indicator on the status bar.
  - Audio pitch dynamically scales with speed (tape-style playback).
  - *Note*: Pressing the Left Stick to change speed prevents the input from reaching the game.

---

### 📂 Setup & File Structure

- **Configuration File**: Settings are saved in `shocora.toml` within the working directory.
- **ROM Directory Management**: Listed under `rom_dirs` in `shocora.toml`. An old `roms.json` is read once when `rom_dirs` is empty.
  - Supports extracted titles (`<name>/code`, `content`, `meta`) and `.rpx` files.
  - Detected games appear in the **Games** panel; double-click to boot.
- **Updates**: An installed update is read from Wii U system storage laid out like Cemu's `mlc01` (`usr/title/0005000e/<id>/`). Set `mlc_dir` in `shocora.toml`; Cemu's own `mlc01` is searched after it.
  - `skip_update_check = true` (default) lets titles that refuse to start without an update (e.g. *Breath of the Wild* 0.9.0) start anyway.
- **Shader Cache**: Compiled GPU shaders are saved in `shader_cache/` to minimize stuttering on subsequent runs.
- **Localization**: Change languages in **Settings > General**.
  - English is built into the executable and is the default and fallback. Every other language is read at run time from `lang/translation.<code>.json` in the `lang` folder next to the executable (or, failing that, in the working directory), so texts can be edited without rebuilding. A `lang/translation.eng.json` there overrides the built-in English.
  - **Adding a language**: copy `lang/translation.eng.json` to `lang/translation.<code>.json` (e.g. `translation.fra.json`), translate the values keeping the keys (an LLM can do it), and set `"language.name"` to the language's own name. It appears in **Settings > General**; missing texts show in English.

---

### 🌐 Local Network Netplay

Allows a second player to join via local network or a VPN such as Radmin VPN.

- **Host Setup**: Hosts on port `47800` at `127.0.0.1` by default, which only this PC can join. To play with another PC, enter this PC's address on the network the guest is on (e.g., `192.168.1.10`, or its VPN address).
  - Access is limited to this PC and guests within that address's network. The network's size comes from the adapter; `10.1.2.3/16` overrides it.
  - With such an address the port listens on all of the PC's addresses. Guests on other PCs need inbound TCP `47800` allowed through the firewall.
  - The addresses entered are saved to `shocora.toml`, which is git-ignored.
  - **Give player 2 the rights of player 1** lets the guest play on the GamePad while your own input does not reach the game. The rights end when the guest leaves.
  - Password protection is optional.
- **Guest Experience**: Connect to the host's IP (`127.0.0.1` on the same PC, the default) to play as a **Pro Controller**. The picture starts once the host runs a title.
  - **Video**: TV at 640 × 360 and 20 fps, GamePad on its right at 427 × 240 and 10 fps.
  - **GamePad**: The mouse touches its screen. **Send buttons to the GamePad** moves the controller from the Pro Controller to the GamePad.
  - **Status Bar**: Shows the host's FPS, title and so on, updated once per second.
  - **Audio**: 16 kHz stream.
- **Connection Handling**: Ping checks occur once per second. If no response is received for 3 seconds, the connection closes and the guest continuously attempts reconnection.
- **Security Warning**: While passwords are not sent in plain text, network traffic is **unencrypted**. Only use Netplay on trusted networks.

---

### 🛠 Debugging

Run headless boots for debugging via command line:

```bash
cargo run --release -p shocora_tests --example headless -- <TITLE_DIR> [FRAMES]
```

## References

- [docs/ref-wiiubrew](docs/ref-wiiubrew/01_emulator_architecture.md)
- [Wii U Architecture](https://www.copetti.org/writings/consoles/wiiu/)
- [Wii U Home Brew](https://wiiubrew.org/wiki/Main_Page)
- [Cemu](https://github.com/cemu-project/Cemu): Wii U Emulator written in  C++

## License

- SPDX-FileCopyrightText: (C) 2026 Cemu Project
- SPDX-License-Identifier: MPL-2.0
