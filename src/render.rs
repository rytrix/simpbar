//! Drawing primitives. All coordinates are logical pixels; the canvas
//! multiplies by the output scale.

use std::sync::Arc;

use cosmic_text::{
    Attrs, Buffer, Ellipsize, EllipsizeHeightLimit, Family, FontSystem, Hinting, Metrics, Shaping, SwashCache, Wrap,
};
use resvg::tiny_skia::{Color, FillRule, Paint, Path, PathBuilder, Pixmap, PixmapPaint, Rect, Stroke, Transform};

use crate::icons::{IconCache, IconSource};

pub struct Canvas {
    pub pixmap: Pixmap,
    pub scale: f32,
}

pub fn rgb(hex: u32, opacity: f32) -> Color {
    Color::from_rgba8((hex >> 16) as u8, (hex >> 8) as u8, hex as u8, (opacity * 255.0).round() as u8)
}

impl Canvas {
    /// `width`/`height` are logical; returns None for an empty surface.
    pub fn new(width: u32, height: u32, scale: u32) -> Option<Canvas> {
        Some(Canvas { pixmap: Pixmap::new(width * scale, height * scale)?, scale: scale as f32 })
    }

    fn transform(&self) -> Transform {
        Transform::from_scale(self.scale, self.scale)
    }

    pub fn fill(&mut self, color: Color) {
        self.pixmap.fill(color);
    }

    pub fn fill_rect(&mut self, x: f32, y: f32, w: f32, h: f32, color: Color) {
        if let Some(rect) = Rect::from_xywh(x, y, w, h) {
            let paint = paint(color);
            let t = self.transform();
            self.pixmap.fill_rect(rect, &paint, t, None);
        }
    }

    pub fn fill_rounded(&mut self, x: f32, y: f32, w: f32, h: f32, r: f32, color: Color) {
        if let Some(path) = rounded_rect(x, y, w, h, r) {
            let t = self.transform();
            self.pixmap.fill_path(&path, &paint(color), FillRule::Winding, t, None);
        }
    }

    /// Draws a 1px border just inside the box, like a QML Rectangle border.
    pub fn stroke_rounded(&mut self, x: f32, y: f32, w: f32, h: f32, r: f32, color: Color) {
        let width = 1.0;
        let i = width / 2.0;
        if let Some(path) = rounded_rect(x + i, y + i, w - width, h - width, r - i) {
            let t = self.transform();
            let stroke = Stroke { width, ..Default::default() };
            self.pixmap.stroke_path(&path, &paint(color), &stroke, t, None);
        }
    }

    /// Draws a line `length` long centered on (cx, cy), rotated `degrees` clockwise.
    pub fn line(&mut self, cx: f32, cy: f32, length: f32, width: f32, degrees: f32, color: Color) {
        let (s, c) = degrees.to_radians().sin_cos();
        let (dx, dy) = (c * length / 2.0, s * length / 2.0);
        let mut pb = PathBuilder::new();
        pb.move_to(cx - dx, cy - dy);
        pb.line_to(cx + dx, cy + dy);
        if let Some(path) = pb.finish() {
            let t = self.transform();
            let stroke = Stroke { width, line_cap: resvg::tiny_skia::LineCap::Round, ..Default::default() };
            self.pixmap.stroke_path(&path, &paint(color), &stroke, t, None);
        }
    }

    /// Fills the triangle through three points.
    pub fn triangle(&mut self, points: [(f32, f32); 3], color: Color) {
        let mut pb = PathBuilder::new();
        pb.move_to(points[0].0, points[0].1);
        pb.line_to(points[1].0, points[1].1);
        pb.line_to(points[2].0, points[2].1);
        pb.close();
        if let Some(path) = pb.finish() {
            let t = self.transform();
            self.pixmap.fill_path(&path, &paint(color), FillRule::Winding, t, None);
        }
    }

    /// Draws the first loadable icon into a `size`×`size` logical box.
    pub fn icon(&mut self, icons: &mut IconCache, sources: &[IconSource], x: f32, y: f32, size: f32, opacity: f32) {
        let px = (size * self.scale).round() as u32;
        let Some(img) = icons.first(sources, px) else { return };
        let paint = PixmapPaint { opacity, ..Default::default() };
        let (x, y) = ((x * self.scale).round() as i32, (y * self.scale).round() as i32);
        self.pixmap.draw_pixmap(x, y, img.as_ref(), &paint, Transform::identity(), None);
    }

    /// Copies into a wl_shm ARGB8888 buffer (little-endian BGRA, premultiplied).
    pub fn copy_to_shm(&self, dst: &mut [u8]) {
        for (d, s) in dst.as_chunks_mut::<4>().0.iter_mut().zip(self.pixmap.data().as_chunks::<4>().0) {
            *d = [s[2], s[1], s[0], s[3]];
        }
    }
}

fn paint(color: Color) -> Paint<'static> {
    let mut p = Paint::default();
    p.set_color(color);
    p.anti_alias = true;
    p
}

fn rounded_rect(x: f32, y: f32, w: f32, h: f32, r: f32) -> Option<Path> {
    let r = r.min(w / 2.0).min(h / 2.0).max(0.0);
    // Cubic approximation of a quarter circle.
    let k = r * 0.552_284_8;
    let mut pb = PathBuilder::new();
    pb.move_to(x + r, y);
    pb.line_to(x + w - r, y);
    pb.cubic_to(x + w - r + k, y, x + w, y + r - k, x + w, y + r);
    pb.line_to(x + w, y + h - r);
    pb.cubic_to(x + w, y + h - r + k, x + w - r + k, y + h, x + w - r, y + h);
    pb.line_to(x + r, y + h);
    pb.cubic_to(x + r - k, y + h, x, y + h - r + k, x, y + h - r);
    pb.line_to(x, y + r);
    pb.cubic_to(x, y + r - k, x + r - k, y, x + r, y);
    pb.close();
    pb.finish()
}

const FONT: &[u8] = include_bytes!("../assets/fonts/AdwaitaSans-Regular.ttf");

/// Shapes and rasterizes text with the bundled font, the only one loaded.
pub struct Text {
    fonts: FontSystem,
    cache: SwashCache,
    family: String,
}

/// Shaped text ready to draw at the scale it was shaped for.
pub struct Shaped {
    buffer: Buffer,
    /// Logical width.
    pub width: f32,
    /// Logical line height.
    pub height: f32,
    scale: f32,
}

impl Text {
    pub fn new() -> Text {
        let mut db = fontdb::Database::new();
        db.load_font_source(fontdb::Source::Binary(Arc::new(FONT)));
        let family = db.faces().next().expect("bundled font parses").families[0].0.clone();
        Text { fonts: FontSystem::new_with_locale_and_db("en-US".into(), db), cache: SwashCache::new(), family }
    }

    /// Shapes a single line of `size`px text, ellipsized past `max_width`.
    pub fn shape(&mut self, text: &str, size: f32, max_width: Option<f32>, scale: f32) -> Shaped {
        let line_height = (size * 1.2).round();
        let mut buffer = Buffer::new(&mut self.fonts, Metrics::new(size * scale, line_height * scale));
        buffer.set_wrap(Wrap::None);
        buffer.set_hinting(Hinting::Enabled);
        if let Some(max) = max_width {
            buffer.set_ellipsize(Ellipsize::End(EllipsizeHeightLimit::Lines(1)));
            buffer.set_size(Some(max.max(0.0) * scale), None);
        }
        let attrs = Attrs::new().family(Family::Name(&self.family));
        buffer.set_text(text, &attrs, Shaping::Advanced, None);
        buffer.shape_until_scroll(&mut self.fonts, false);
        let width = buffer.layout_runs().map(|r| r.line_w).fold(0.0, f32::max) / scale;
        Shaped { buffer, width, height: line_height, scale }
    }

    /// Draws shaped text starting at `x`, its line box vertically centered on `mid`.
    pub fn draw(&mut self, canvas: &mut Canvas, shaped: &mut Shaped, x: f32, mid: f32, color: Color) {
        debug_assert_eq!(shaped.scale, canvas.scale);
        let y = mid - shaped.height / 2.0;
        let ox = (x * canvas.scale).round() as i32;
        let oy = (y * canvas.scale).round() as i32;
        let (pw, ph) = (canvas.pixmap.width() as i32, canvas.pixmap.height() as i32);
        let data = canvas.pixmap.data_mut();
        let src = color.to_color_u8();
        let fg = cosmic_text::Color::rgba(src.red(), src.green(), src.blue(), 255);
        let opacity = src.alpha() as u32;
        shaped.buffer.draw(&mut self.fonts, &mut self.cache, fg, |gx, gy, w, h, c| {
            let a = c.a() as u32 * opacity / 255;
            if a == 0 {
                return;
            }
            for py in (oy + gy).max(0)..(oy + gy + h as i32).min(ph) {
                for px in (ox + gx).max(0)..(ox + gx + w as i32).min(pw) {
                    let i = ((py * pw + px) * 4) as usize;
                    let d = &mut data[i..i + 4];
                    let inv = 255 - a;
                    let blend = |s: u8, d: u8| ((s as u32 * a + d as u32 * inv + 127) / 255) as u8;
                    d[0] = blend(c.r(), d[0]);
                    d[1] = blend(c.g(), d[1]);
                    d[2] = blend(c.b(), d[2]);
                    d[3] = ((a * 255 + d[3] as u32 * inv + 127) / 255) as u8;
                }
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loads_only_the_bundled_font() {
        let mut text = Text::new();
        assert_eq!(text.fonts.db().faces().count(), 1);
        assert_eq!(text.family, "Adwaita Sans");
        assert!(text.shape("Thu 10/22/26 2:18 PM", 16.0, None, 1.0).width > 100.0);
    }
}
