# simpbar

A minimal Wayland status bar for [niri](https://github.com/YaLTeR/niri), ported
from the Quickshell config in `quickshell/bar`.

- Workspaces for the bar's output (click to focus)
- Clock (click opens `gnome-calendar`)
- StatusNotifierItem tray with an inline-expanding context menu
- Command-driven modules: network, bluetooth, mic, volume, brightness, battery

## Build & run

```sh
cargo build --release
./target/release/simpbar
```

Runtime tools used by the default modules: `nmcli`, `bluetoothctl`,
`dbus-monitor`, `wpctl`, `pactl`, `brightnessctl`, `pavucontrol`,
`blueman-manager`. Logs go to stderr; set `RUST_LOG=debug` for more.

## Configuration

Edit `src/config.rs` and rebuild: colors, sizes, the clock format and the module
list live there. Each module has a `command` (polled every `interval`, and
re-run whenever its `watch` command prints a line), optional `click` /
`scroll_up` / `scroll_down` commands, and a `format` function that turns the
command's output into label text, dimming, icon overrides and a mute slash.

Text uses `res/AdwaitaSans-Regular.ttf`, compiled into the binary; no system
fonts are loaded. To change it, replace the file and the path in `src/render.rs`.

To preview a render without restarting the bar:

```sh
SIMPBAR_PREVIEW=/tmp cargo test   # writes /tmp/bar@1x.png, bar@2x.png, menu@2x.png
```
