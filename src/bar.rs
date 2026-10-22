//! Layout and drawing of one bar: workspaces on the left, the clock in the
//! middle, tray and modules on the right.

use chrono::{DateTime, Local};

use crate::config::*;
use crate::icons::{IconCache, IconSource};
use crate::modules::{Module, Runner};
use crate::niri::Workspace;
use crate::render::{Canvas, Text, rgb};
use crate::tray::TrayItem;

pub struct ModuleSlot {
    pub config: Module,
    pub raw: String,
    pub runner: Runner,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Target {
    Workspace(u64),
    Clock,
    Module(usize),
    Tray(String),
}

/// A clickable region; spans the full bar height.
#[derive(Debug, Clone)]
pub struct Hit {
    pub x: f32,
    pub w: f32,
    pub target: Target,
}

pub fn hit_test(hits: &[Hit], x: f64) -> Option<&Hit> {
    let x = x as f32;
    hits.iter().find(|h| x >= h.x && x < h.x + h.w)
}

pub struct Context<'a> {
    pub text: &'a mut Text,
    pub icons: &'a mut IconCache,
    pub modules: &'a [ModuleSlot],
    pub workspaces: &'a [Workspace],
    pub tray: &'a [TrayItem],
    pub now: DateTime<Local>,
}

const ICON: f32 = 16.0;

pub fn draw(canvas: &mut Canvas, cx: &mut Context, width: f32, output: &str) -> Vec<Hit> {
    let mid = BAR_HEIGHT as f32 / 2.0;
    let scale = canvas.scale;
    let mut hits = Vec::new();
    canvas.fill(rgb(BACKGROUND, 1.0));

    let mut x = MARGIN;
    for ws in cx.workspaces.iter().filter(|w| w.output.as_deref() == Some(output)) {
        const W: f32 = 14.0;
        let mut label = cx.text.shape(&ws.idx.to_string(), FONT_SIZE, None, scale);
        let color = if ws.is_active { FOREGROUND } else { INACTIVE };
        let tx = x + ((W - label.width) / 2.0).round();
        cx.text.draw(canvas, &mut label, tx, mid, rgb(color, 1.0));
        hits.push(Hit { x, w: W, target: Target::Workspace(ws.id) });
        x += W + 4.0;
    }

    let mut clock = cx.text.shape(&cx.now.format(CLOCK_FORMAT).to_string(), FONT_SIZE, None, scale);
    let tx = ((width - clock.width) / 2.0).round();
    cx.text.draw(canvas, &mut clock, tx, mid, rgb(FOREGROUND, 1.0));
    hits.push(Hit { x: tx, w: clock.width, target: Target::Clock });

    let mut x = width - MARGIN;
    for (i, slot) in cx.modules.iter().enumerate().rev() {
        let m = &slot.config;
        let state = (m.format)(&slot.raw);
        if m.hide_when_empty && state.text.is_empty() {
            continue;
        }
        let opacity = if state.dim { DIM_OPACITY } else { 1.0 };
        let mut label = m.show_label.then(|| cx.text.shape(&state.text, FONT_SIZE, Some(m.max_label_width), scale));
        let w = ICON + label.as_ref().map_or(0.0, |l| 4.0 + l.width);
        x -= w;

        let icon = IconSource::Builtin(state.icon.unwrap_or(m.icon));
        canvas.icon(cx.icons, std::slice::from_ref(&icon), x, mid - ICON / 2.0, ICON, opacity);
        if state.slash {
            canvas.line(x + ICON / 2.0, mid, 20.0, 1.5, 45.0, rgb(FOREGROUND, 1.0));
        }
        if let Some(label) = &mut label {
            cx.text.draw(canvas, label, x + ICON + 4.0, mid, rgb(FOREGROUND, opacity));
        }
        hits.push(Hit { x, w, target: Target::Module(i) });
        x -= 6.0;
    }

    if !cx.tray.is_empty() {
        const SPACING: f32 = 8.0;
        x -= cx.tray.len() as f32 * (ICON + SPACING) - SPACING;
        for item in cx.tray {
            canvas.icon(cx.icons, &item.icon, x, mid - ICON / 2.0, ICON, 1.0);
            hits.push(Hit { x, w: ICON, target: Target::Tray(item.address.clone()) });
            x += ICON + SPACING;
        }
    }

    hits
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::niri::Workspace;

    /// Renders a bar with sample data. Set SIMPBAR_PREVIEW=dir to save PNGs.
    #[test]
    fn renders_preview() {
        let (tx, _rx) = calloop::channel::channel();
        let raws = ["wifi:connected", "1", "Volume: 0.42 [MUTED]", "Volume: 0.65", "80%", "93"];
        let modules: Vec<_> = crate::config::modules()
            .into_iter()
            .zip(raws)
            .map(|(config, raw)| ModuleSlot {
                config,
                raw: raw.into(),
                runner: Runner::spawn(0, &Module::DEFAULT, tx.clone()),
            })
            .collect();
        let ws = |id, idx, is_active| Workspace { id, idx, output: Some("DP-1".into()), is_active };
        let workspaces = [ws(1, 1, false), ws(2, 2, true), ws(3, 3, false)];
        let mut text = Text::new();
        let mut icons = IconCache::default();
        for scale in [1, 2] {
            let mut canvas = Canvas::new(800, BAR_HEIGHT, scale).unwrap();
            let mut cx = Context {
                text: &mut text,
                icons: &mut icons,
                modules: &modules,
                workspaces: &workspaces,
                tray: &[],
                now: chrono::Local::now(),
            };
            let hits = draw(&mut canvas, &mut cx, 800.0, "DP-1");
            assert_eq!(hits.iter().filter(|h| matches!(h.target, Target::Workspace(_))).count(), 3);
            assert_eq!(hits.iter().filter(|h| matches!(h.target, Target::Module(_))).count(), 6);
            assert!(hit_test(&hits, 400.0).is_some_and(|h| h.target == Target::Clock));
            assert!(canvas.pixmap.pixels().iter().any(|p| p.red() > 200));
            if let Some(dir) = std::env::var_os("SIMPBAR_PREVIEW") {
                canvas.pixmap.save_png(std::path::Path::new(&dir).join(format!("bar@{scale}x.png"))).unwrap();
            }
        }
    }
}
