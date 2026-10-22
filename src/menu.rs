//! Tray context menu contents. Submenus expand inline beneath their parent.

use std::collections::HashSet;

use system_tray::menu::{MenuItem, MenuType, ToggleState, ToggleType};

use crate::config::*;
use crate::icons::{IconCache, IconSource};
use crate::render::{Canvas, Text, rgb};

const PADDING: f32 = 4.0;
const ROW_HEIGHT: f32 = 26.0;
const SEPARATOR_HEIGHT: f32 = 9.0;

pub struct Row<'a> {
    pub item: &'a MenuItem,
    /// Relative to the top of the list (inside the panel padding).
    pub y: f32,
    pub h: f32,
    check_width: f32,
    icon_width: f32,
}

impl Row<'_> {
    pub fn is_separator(&self) -> bool {
        self.item.menu_type == MenuType::Separator
    }

    pub fn has_children(&self) -> bool {
        !self.item.submenu.is_empty() || self.item.children_display.as_deref() == Some("submenu")
    }
}

pub fn layout<'a>(items: &'a [MenuItem], expanded: &HashSet<i32>) -> Vec<Row<'a>> {
    let mut rows = Vec::new();
    let mut y = 0.0;
    push_level(items, expanded, &mut y, &mut rows);
    rows
}

fn push_level<'a>(items: &'a [MenuItem], expanded: &HashSet<i32>, y: &mut f32, rows: &mut Vec<Row<'a>>) {
    let items: Vec<_> = items.iter().filter(|i| i.visible).collect();
    // Each level reserves check/icon columns only if one of its items needs them.
    let check_width = if items.iter().any(|i| i.toggle_type != ToggleType::CannotBeToggled) { 18.0 } else { 0.0 };
    let icon_width =
        if items.iter().any(|i| i.icon_name.as_deref().is_some_and(|n| !n.is_empty()) || i.icon_data.is_some()) {
            22.0
        } else {
            0.0
        };
    for item in items {
        let h = if item.menu_type == MenuType::Separator { SEPARATOR_HEIGHT } else { ROW_HEIGHT };
        rows.push(Row { item, y: *y, h, check_width, icon_width });
        *y += h;
        if expanded.contains(&item.id) {
            push_level(&item.submenu, expanded, y, rows);
        }
    }
}

/// Logical size of the panel holding `rows`.
pub fn size(rows: &[Row]) -> (f32, f32) {
    let h = rows.last().map_or(0.0, |r| r.y + r.h);
    (MENU_WIDTH, h + 2.0 * PADDING)
}

/// Finds the row under a point in panel coordinates.
pub fn row_at<'r, 'a>(rows: &'r [Row<'a>], x: f64, y: f64) -> Option<&'r Row<'a>> {
    let (x, y) = (x as f32 - PADDING, y as f32 - PADDING);
    if x < 0.0 || x >= MENU_WIDTH - 2.0 * PADDING {
        return None;
    }
    rows.iter().find(|r| y >= r.y && y < r.y + r.h)
}

pub fn draw(
    canvas: &mut Canvas,
    text: &mut Text,
    icons: &mut IconCache,
    rows: &[Row],
    expanded: &HashSet<i32>,
    hovered: Option<i32>,
) {
    let (w, h) = size(rows);
    let scale = canvas.scale;
    canvas.fill(rgb(0, 0.0));
    canvas.fill_rounded(0.0, 0.0, w, h, 4.0, rgb(MENU_BACKGROUND, 1.0));
    canvas.stroke_rounded(0.0, 0.0, w, h, 4.0, rgb(MENU_BORDER, 1.0));

    let row_w = w - 2.0 * PADDING;
    for row in rows {
        let (x, y) = (PADDING, PADDING + row.y);
        let mid = y + row.h / 2.0;
        let item = row.item;
        if row.is_separator() {
            canvas.fill_rect(x + 8.0, (mid - 0.5).floor(), row_w - 16.0, 1.0, rgb(MENU_BORDER, 1.0));
            continue;
        }
        if item.enabled && hovered == Some(item.id) {
            canvas.fill_rounded(x, y, row_w, row.h, 3.0, rgb(MENU_HOVER, 1.0));
        }
        let opacity = if item.enabled { 1.0 } else { 0.4 };
        let fg = rgb(FOREGROUND, opacity);

        if item.toggle_type != ToggleType::CannotBeToggled && item.toggle_state != ToggleState::Off {
            let mark = if item.toggle_type == ToggleType::Radio { "●" } else { "✓" };
            let mut s = text.shape(mark, 14.0, None, scale);
            text.draw(canvas, &mut s, x + 8.0, mid, fg);
        }

        let icon = match (&item.icon_data, &item.icon_name) {
            (Some(data), _) => Some(IconSource::png(data.clone())),
            (None, Some(name)) if !name.is_empty() => Some(IconSource::Named { name: name.clone(), theme_path: None }),
            _ => None,
        };
        if let Some(icon) = icon {
            canvas.icon(icons, std::slice::from_ref(&icon), x + 8.0 + row.check_width, mid - 8.0, 16.0, opacity);
        }

        let lx = 8.0 + row.check_width + row.icon_width;
        let label = mnemonic_free(item.label.as_deref().unwrap_or(""));
        let mut s = text.shape(&label, FONT_SIZE, Some(row_w - lx - 24.0), scale);
        text.draw(canvas, &mut s, x + lx, mid, fg);

        if row.has_children() {
            // Drawn rather than ▸/▾ glyphs, which the bundled font lacks.
            let right = x + row_w - 8.0;
            if expanded.contains(&item.id) {
                canvas.triangle([(right - 6.0, mid - 1.5), (right, mid - 1.5), (right - 3.0, mid + 2.0)], fg);
            } else {
                canvas.triangle([(right - 4.0, mid - 3.0), (right, mid), (right - 4.0, mid + 3.0)], fg);
            }
        }
    }
}

/// Strips mnemonic markers: "_File" → "File", "a__b" → "a_b".
fn mnemonic_free(label: &str) -> String {
    let mut out = String::with_capacity(label.len());
    let mut chars = label.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '_' {
            if chars.peek() == Some(&'_') {
                out.push('_');
                chars.next();
            }
        } else {
            out.push(c);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_mnemonics() {
        assert_eq!(mnemonic_free("_Quit"), "Quit");
        assert_eq!(mnemonic_free("snake__case"), "snake_case");
        assert_eq!(mnemonic_free("plain"), "plain");
    }

    #[test]
    fn expands_submenus_inline() {
        let leaf = MenuItem { id: 3, visible: true, enabled: true, label: Some("leaf".into()), ..Default::default() };
        let parent = MenuItem { id: 2, visible: true, enabled: true, submenu: vec![leaf], ..Default::default() };
        let sep = MenuItem { id: 4, visible: true, menu_type: MenuType::Separator, ..Default::default() };
        let hidden = MenuItem { id: 5, visible: false, ..Default::default() };
        let items = vec![parent, sep, hidden];

        let collapsed = layout(&items, &HashSet::new());
        assert_eq!(collapsed.iter().map(|r| r.item.id).collect::<Vec<_>>(), [2, 4]);
        assert_eq!(size(&collapsed).1, ROW_HEIGHT + SEPARATOR_HEIGHT + 2.0 * PADDING);

        let open = layout(&items, &HashSet::from([2]));
        assert_eq!(open.iter().map(|r| r.item.id).collect::<Vec<_>>(), [2, 3, 4]);
        assert_eq!(open[2].y, 2.0 * ROW_HEIGHT);
        assert_eq!(row_at(&open, 20.0, (PADDING + ROW_HEIGHT + 1.0) as f64).map(|r| r.item.id), Some(3));
    }
}

#[cfg(test)]
mod preview {
    use super::*;

    #[test]
    fn renders_preview() {
        let item = |id, label: &str| MenuItem {
            id,
            visible: true,
            enabled: true,
            label: Some(label.into()),
            ..Default::default()
        };
        let items = vec![
            MenuItem { toggle_type: ToggleType::Checkmark, toggle_state: ToggleState::On, ..item(1, "_Show window") },
            MenuItem { submenu: vec![item(5, "Nested entry"), item(6, "Another one")], ..item(2, "Submenu") },
            MenuItem { menu_type: MenuType::Separator, ..item(3, "") },
            MenuItem { enabled: false, ..item(7, "Disabled") },
            item(4, "A rather long label that should be elided"),
        ];
        let expanded = HashSet::from([2]);
        let rows = layout(&items, &expanded);
        let (w, h) = size(&rows);
        let mut text = Text::new();
        let mut canvas = Canvas::new(w.ceil() as u32, h.ceil() as u32, 2).unwrap();
        draw(&mut canvas, &mut text, &mut IconCache::default(), &rows, &expanded, Some(5));
        if let Some(dir) = std::env::var_os("SIMPBAR_PREVIEW") {
            canvas.pixmap.save_png(std::path::Path::new(&dir).join("menu@2x.png")).unwrap();
        }
    }
}
