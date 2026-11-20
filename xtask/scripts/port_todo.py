"""Writes docs/todo.md: every HLE export Cemu registers, checked when shocora has it.

Usage (from the repo root):
    cargo test -p wiiu_cafe_os dump_exports -- --ignored --nocapture > exports.txt
    python xtask/scripts/port_todo.py <Cemu src dir> exports.txt docs/todo.md
"""
import os
import re
import sys
from collections import defaultdict

PATTERNS = [
    re.compile(r'cafeExportRegister(?:TF)?\(\s*"([^"]+)"\s*,\s*([A-Za-z_][\w:]*)'),
    re.compile(r'osLib_addFunction\(\s*"([^"]+)"\s*,\s*"([^"]+)"'),
    re.compile(r'cafeExportRegisterFunc\(\s*[^,]+,\s*"([^"]+)"\s*,\s*"([^"]+)"'),
]
# Cemu registers the audio libraries under both names; shocora maps them the same way.
ALIASES = {"snd_core": "sndcore2", "snd_user": "snduser2"}

# Cemu subsystems outside the export tables, by source directory under src/Cafe.
SUBSYSTEMS = [
    ("IOSU/fsa", "FSA (ファイルシステム IPC)"),
    ("IOSU/kernel", "IOSU カーネル (IPC ディスパッチ)"),
    ("IOSU/legacy/iosu_acp.cpp", "ACP (セーブ/タイトル情報)"),
    ("IOSU/legacy/iosu_act.cpp", "ACT (アカウント)"),
    ("IOSU/legacy/iosu_crypto.cpp", "Crypto"),
    ("IOSU/legacy/iosu_fpd.cpp", "FPD (フレンド)"),
    ("IOSU/legacy/iosu_mcp.cpp", "MCP (タイトル管理)"),
    ("IOSU/legacy/iosu_nim.cpp", "NIM (更新)"),
    ("IOSU/nn/boss", "BOSS (SpotPass)"),
    ("IOSU/ODM", "ODM (ディスクドライブ)"),
    ("IOSU/PDM", "PDM (プレイ記録)"),
    ("IOSU/ccr_nfc", "CCR NFC (amiibo)"),
    ("OS/RPL", "RPL ローダー"),
    ("HW/Espresso", "CPU (Espresso)"),
    ("HW/Latte", "GPU (Latte)"),
    ("HW/MMU", "MMU"),
    ("HW/SI", "SI"),
    ("HW/VI", "VI"),
    ("HW/AI", "AI (オーディオ出力)"),
    ("Filesystem", "ファイルシステム (WUD/WUX/WUA/展開済み)"),
    ("Account", "アカウント管理"),
    ("GameProfile", "ゲームプロファイル"),
    ("GraphicPack", "グラフィックパック"),
    ("TitleList", "タイトル一覧"),
    ("GamePatch.cpp", "ゲーム別パッチ"),
]


def cemu_exports(src):
    found = set()
    libs = os.path.join(src, "Cafe", "OS", "libs")
    for root, _, files in os.walk(libs):
        for f in files:
            if not f.endswith(".cpp"):
                continue
            with open(os.path.join(root, f), encoding="utf-8", errors="ignore") as fh:
                text = fh.read()
            rel = os.path.relpath(os.path.join(root, f), libs).replace("\\", "/")
            for pattern in PATTERNS:
                for m in pattern.finditer(text):
                    lib = ALIASES.get(m.group(1), m.group(1))
                    found.add((lib, m.group(2).split("::")[-1], rel))
    return found


def shocora_exports(path):
    have = set()
    with open(path, encoding="utf-8", errors="ignore") as fh:
        for line in fh:
            parts = line.split()
            if len(parts) == 3 and parts[0] == "EXPORT":
                have.add((ALIASES.get(parts[1], parts[1]), parts[2]))
    return have


def main():
    src, exports, out = sys.argv[1:4]
    cemu = cemu_exports(src)
    have = shocora_exports(exports)
    by_lib = defaultdict(dict)
    for lib, name, rel in cemu:
        by_lib[lib].setdefault(name, rel)
    total = sum(len(v) for v in by_lib.values())
    done = sum(1 for lib, v in by_lib.items() for n in v if (lib, n) in have)
    lines = [
        "# Cemu 移植 TODO",
        "",
        "Cemu (`src/Cafe`) の機能を shocora (`core/`) へ移植する作業の一覧。",
        "",
        "- HLE export 表は `xtask/scripts/port_todo.py` で生成する。チェック済みは shocora の",
        "  レジストリに登録済みの関数。再生成の手順はスクリプト冒頭を参照。",
        "- 登録済みでも、中身が Cemu より簡略な関数がある。動作の一致は個別に確認する。",
        "",
        f"## 進捗: HLE export {done} / {total}",
        "",
        "| ライブラリ | 実装済み | 全体 |",
        "| --- | --- | --- |",
    ]
    for lib in sorted(by_lib, key=str.lower):
        names = by_lib[lib]
        n = sum(1 for x in names if (lib, x) in have)
        lines.append(f"| [{lib}](#lib-{lib.lower()}) | {n} | {len(names)} |")
    lines += ["", "## Cemu サブシステム", ""]
    lines.append("export 表以外の Cemu 実装。shocora 側の設計に合わせて移植する。")
    lines.append("")
    for path, label in SUBSYSTEMS:
        lines.append(f"- [ ] {label} (`src/Cafe/{path}`)")
    lines += ["", "## HLE export", ""]
    for lib in sorted(by_lib, key=str.lower):
        names = by_lib[lib]
        lines.append(f'### <a id="lib-{lib.lower()}"></a>{lib}')
        lines.append("")
        for name in sorted(names, key=lambda x: (names[x], x)):
            mark = "x" if (lib, name) in have else " "
            lines.append(f"- [{mark}] `{name}` ({names[name]})")
        lines.append("")
    with open(out, "w", encoding="utf-8", newline="\n") as fh:
        fh.write("\n".join(lines))
    print(f"{done}/{total}")


if __name__ == "__main__":
    main()
