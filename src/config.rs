//! Everything you'd normally tweak: sizes, colors and the right-hand modules.

use std::time::Duration;

use crate::icons::Builtin;
use crate::modules::{Module, ModuleState};

pub const BAR_HEIGHT: u32 = 30;
pub const MARGIN: f32 = 8.0;
pub const FONT_SIZE: f32 = 16.0;

pub const BACKGROUND: u32 = 0x000000;
pub const FOREGROUND: u32 = 0xffffff;
pub const INACTIVE: u32 = 0x808080;
pub const DIM_OPACITY: f32 = 0.4;

pub const MENU_WIDTH: f32 = 180.0;
pub const MENU_BACKGROUND: u32 = 0x1a1a1a;
pub const MENU_BORDER: u32 = 0x333333;
pub const MENU_HOVER: u32 = 0x404040;

pub const CLOCK_FORMAT: &str = "%a %m/%d/%y %-I:%M %p";
pub const CLOCK_CLICK: &[&str] = &["gnome-calendar"];

pub fn modules() -> Vec<Module> {
    vec![
        Module {
            icon: Builtin::Wifi,
            show_label: false,
            command: &[
                "sh",
                "-c",
                "nmcli -t -f TYPE,STATE dev | grep -E '^(wifi|ethernet):connected' | sort | head -n1",
            ],
            watch: &["nmcli", "monitor"],
            interval: Duration::from_secs(30),
            hide_when_empty: false,
            format: |r| {
                if r.starts_with("ethernet") {
                    return ModuleState { text: "on".into(), icon: Some(Builtin::Ethernet), ..Default::default() };
                }
                ModuleState {
                    text: if r.is_empty() { "off" } else { "on" }.into(),
                    dim: r.is_empty(),
                    ..Default::default()
                }
            },
            ..Module::DEFAULT
        },
        Module {
            icon: Builtin::Bluetooth,
            show_label: false,
            command: &[
                "sh",
                "-c",
                "if bluetoothctl show | grep -q 'Powered: yes'; then bluetoothctl devices Connected | wc -l; else echo off; fi",
            ],
            click: &["blueman-manager"],
            watch: &[
                "sh",
                "-c",
                "dbus-monitor --system \"type='signal',sender='org.bluez'\" | grep -E --line-buffered 'string \"(Connected|Powered)\"'",
            ],
            interval: Duration::from_secs(30),
            hide_when_empty: false,
            format: |r| match r {
                "off" => ModuleState { text: "off".into(), dim: true, ..Default::default() },
                "0" => ModuleState { text: "on".into(), ..Default::default() },
                _ => ModuleState { text: r.into(), ..Default::default() },
            },
            ..Module::DEFAULT
        },
        Module {
            icon: Builtin::Mic,
            command: &["wpctl", "get-volume", "@DEFAULT_AUDIO_SOURCE@"],
            click: &["pavucontrol", "-t", "4"],
            scroll_up: &["wpctl", "set-volume", "-l", "1.0", "@DEFAULT_AUDIO_SOURCE@", "1%+"],
            scroll_down: &["wpctl", "set-volume", "@DEFAULT_AUDIO_SOURCE@", "1%-"],
            watch: &["sh", "-c", "pactl subscribe | grep --line-buffered -E 'on (source #|server)'"],
            interval: Duration::from_secs(30),
            format: |r| {
                let muted = r.contains("MUTED");
                ModuleState { slash: muted, ..volume(r) }
            },
            ..Module::DEFAULT
        },
        Module {
            icon: Builtin::Volume,
            command: &["wpctl", "get-volume", "@DEFAULT_AUDIO_SINK@"],
            click: &["pavucontrol", "-t", "3"],
            scroll_up: &["wpctl", "set-volume", "-l", "1.0", "@DEFAULT_AUDIO_SINK@", "1%+"],
            scroll_down: &["wpctl", "set-volume", "@DEFAULT_AUDIO_SINK@", "1%-"],
            watch: &["sh", "-c", "pactl subscribe | grep --line-buffered -E 'on (sink #|server)'"],
            interval: Duration::from_secs(30),
            format: volume,
            ..Module::DEFAULT
        },
        Module {
            icon: Builtin::Brightness,
            command: &["sh", "-c", "brightnessctl -m 2>/dev/null | cut -d, -f4"],
            scroll_up: &["brightnessctl", "set", "1%+"],
            scroll_down: &["brightnessctl", "set", "1%-"],
            interval: Duration::from_secs(5),
            ..Module::DEFAULT
        },
        Module {
            icon: Builtin::Battery,
            // Whichever battery exists (BAT0, BAT1, ...); the first if there are several.
            command: &["sh", "-c", "cat /sys/class/power_supply/BAT*/capacity 2>/dev/null | head -n1"],
            interval: Duration::from_secs(30),
            format: |r| ModuleState {
                text: if r.is_empty() { String::new() } else { format!("{r}%") },
                ..Default::default()
            },
            ..Module::DEFAULT
        },
    ]
}

/// Formats `wpctl get-volume` output, e.g. "Volume: 0.45 [MUTED]".
fn volume(r: &str) -> ModuleState {
    if r.is_empty() {
        return ModuleState::default();
    }
    let level = r.split_whitespace().find_map(|w| w.parse::<f64>().ok()).unwrap_or(0.0);
    ModuleState { text: format!("{}%", (level * 100.0).round() as i64), dim: r.contains("MUTED"), ..Default::default() }
}
