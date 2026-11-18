# Nesting

Lays out laser and router parts on sheets and remnants to use as little fresh material as possible.

## Install on Windows

Needs 64-bit Windows 10 or 11.

1. Download [Nesting.exe](https://github.com/hussainph/nesting/releases/latest/download/Nesting.exe).
2. Move it somewhere it can stay, like the Desktop, and double-click it. There's nothing else to install.
3. The first time you open it, Windows may show "Windows protected your PC" because the app isn't signed. Click **More info**, then **Run anyway**.

Nesting updates itself. It checks for a new version when it starts and every few hours after, and installs it in the background. **Restart to update** then appears in the toolbar. Copies older than 0.1.2 can't update themselves, so replace those with a fresh download once. Your stock library is kept either way.

If an update can't be installed, usually because Nesting.exe sits in a folder Windows protects, like Program Files, the app says so. Keep it on the Desktop or in your own folder. To turn updates off on a machine, set the environment variable `NESTING_NO_UPDATE` to `1`.

## Run from source

```sh
cargo run --release -p nest-app        # the desktop app
cargo run --release -p nest-cli -- --help
```

Rust 1.99.1 is pinned in `rust-toolchain.toml`. rustup installs it on first build.

## Using the app

1. Add parts. Drop AI, EPS, PDF, DXF or SVG files on the window, or use **Add files**. Every closed outline becomes a part. Holes stay with their part, and the separate dots of Arabic letters become their own parts.
2. Set quantities in the parts list.
3. Pick stock. Tick the sheets and remnants to use. Use **+** next to Stock to draw a new sheet or remnant on the canvas, or the pencil on a row to edit one. Click a sheet on the board to select it, then rotate it with the round handle, the bar's rotate buttons, or R and Shift+R. The nest fills each sheet from the left as it lies on the board.
4. **Nest** (Ctrl+Enter). The best layout so far shows while it runs. **Stop** keeps it.
5. **Export** writes one DXF or SVG per sheet. Layers: `CUT` (black), `MARK` (red, engraving), `SHEET` (blue, the stock outline).
6. **Save offcut** on a sheet adds the unused end of that sheet to stock as a new remnant.

### Adjusting a nest by hand

Click a part to select it. Shift-click adds to the selection, and dragging a box on a sheet selects everything inside it.

- Drag a part to move it, including onto another sheet. Arrow keys nudge by 1 mm, or 10 mm with Shift.
- Drag the round handle to rotate (hold Shift to snap to 15°). R and Shift+R turn a quarter.
- Drag a square handle to resize. Resizing changes every copy of that part and applies to the next nest.
- Type exact X, Y, width, height or angle in the bar at the top of the board.
- Parts that overlap, sit closer than the spacing, or leave the sheet turn red.
- ⌘Z / Ctrl+Z undoes, Shift+⌘Z redoes.

Pan with a two-finger scroll, a middle-button drag, or by dragging the empty board.

### AI files

Both kinds of .ai file import: current Illustrator files, and the older Illustrator 8 format that CorelDRAW writes when you export to AI. Illustrator .eps files work too. Text isn't imported, so convert it to outlines (curves in CorelDRAW) before saving.

### Shared stock

The stock library is a JSON file:

- Windows: `%APPDATA%\Nesting\library.json`
- macOS: `~/Library/Application Support/Nesting/library.json`
- Linux: `~/.local/share/Nesting/library.json`

Set `NESTING_LIBRARY` to a path on a shared folder so every machine sees the same stock.

## Command line

```sh
nest-cli parts/*.dxf --qty 4 --sheet 1200x900:2 --remnant offcut.dxf --time 20 --out out/ --format dxf
```

## Layout

| Crate | What it does |
| --- | --- |
| `crates/nest-core` | Shared types: parts, stock, settings, results. mm, y-up. |
| `crates/nest-io` | SVG, DXF and AI/PDF import; SVG and DXF export. |
| `crates/nest-engine` | The nester, on [jagua-rs](https://github.com/JeroenGar/jagua-rs) with a port of [sparrow](https://github.com/JeroenGar/sparrow)'s separator. |
| `crates/nest-cli` | Command-line runs. |
| `app` | Desktop app on [Rune](https://github.com/anomalyco/rune). |

Rune is pre-release and pinned to one revision in `Cargo.toml`, along with the crates it patches. Bump them together.

## Checking changes

```sh
cargo test --workspace
cargo test -p nest-engine --release -- --ignored --nocapture bench   # engine benchmarks
target/debug/nesting --snap out.png --scene result                   # headless screenshot
```

Scenes: `empty`, `parts`, `result`, `running`, `select-part`, `select-sheet`, `update-ready`, `add-remnant`, `edit-remnant`, `arabic`, `engine`.

## Releasing

1. Bump `version` in the root `Cargo.toml` and commit.
2. Publish a release with a matching tag: `gh release create v0.2.0 --notes "..."`.

CI builds `Nesting.exe` on Windows and attaches it to the release with its SHA-256 checksum, `Nesting.exe.sha256`. The download link above always points at the newest release, and running copies install it on their next check. The updater refuses a release without the checksum file, so publish releases through CI only.

## Third-party code

`crates/nest-engine/src/sep.rs` is adapted from [sparrow](https://github.com/JeroenGar/sparrow), MIT License, copyright (c) 2025 Jeroen Gardeyn, KU Leuven. The full notice is at the top of that file. [jagua-rs](https://github.com/JeroenGar/jagua-rs) is used as a dependency under the MPL 2.0.
