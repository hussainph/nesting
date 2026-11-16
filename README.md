# Nesting

Lays out laser and router parts on sheets and remnants to use as little fresh material as possible.

## Run it

```sh
cargo run --release -p nest-app        # the desktop app
cargo run --release -p nest-cli -- --help
```

Rust 1.99.1 is pinned in `rust-toolchain.toml`. rustup installs it on first build.

## Using the app

1. Add parts. Drop AI, PDF, DXF or SVG files on the window, or use **Add files**. Every closed outline becomes a part. Holes stay with their part, and the separate dots of Arabic letters become their own parts.
2. Set quantities in the parts list.
3. Pick stock. Tick the sheets and remnants to use. Use **+** next to Stock to add a sheet or remnant by size, by clicking its corners, or from a DXF or SVG outline.
4. **Nest** (Ctrl+Enter). The best layout so far shows while it runs. **Stop** keeps it.
5. **Export** writes one DXF or SVG per sheet. Layers: `CUT` (black), `MARK` (red, engraving), `SHEET` (blue, the stock outline).
6. **Save offcut** on a sheet adds the unused end of that sheet to stock as a new remnant.

### AI files

Illustrator files must be saved with **Create PDF Compatible File** checked (the default). Convert text to outlines before saving.

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

Scenes: `empty`, `parts`, `result`, `running`, `add-remnant`, `draw-remnant`, `arabic`, `engine`.
