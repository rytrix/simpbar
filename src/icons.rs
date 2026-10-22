//! Icon loading: the bundled SVGs, freedesktop theme lookups and the raw
//! ARGB pixmaps that StatusNotifierItems send over D-Bus.

use std::collections::HashMap;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

use resvg::tiny_skia::{IntSize, Pixmap, PixmapPaint, Transform};
use resvg::usvg;
use system_tray::item::IconPixmap;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Builtin {
    Battery,
    Bluetooth,
    Brightness,
    Ethernet,
    Mic,
    Volume,
    Wifi,
}

impl Builtin {
    fn svg(self) -> &'static [u8] {
        match self {
            Builtin::Battery => include_bytes!("../assets/icons/battery.svg"),
            Builtin::Bluetooth => include_bytes!("../assets/icons/bluetooth.svg"),
            Builtin::Brightness => include_bytes!("../assets/icons/brightness.svg"),
            Builtin::Ethernet => include_bytes!("../assets/icons/ethernet.svg"),
            Builtin::Mic => include_bytes!("../assets/icons/mic.svg"),
            Builtin::Volume => include_bytes!("../assets/icons/volume.svg"),
            Builtin::Wifi => include_bytes!("../assets/icons/wifi.svg"),
        }
    }
}

/// Where an icon comes from; resolved lazily at whatever size is drawn.
#[derive(Debug, Clone)]
pub enum IconSource {
    Builtin(Builtin),
    /// Theme icon name or absolute path, plus an optional extra search dir.
    Named {
        name: String,
        theme_path: Option<String>,
    },
    /// `hash` is computed once up front so redraws don't rehash the pixels.
    Pixmaps {
        hash: u64,
        data: Arc<Vec<IconPixmap>>,
    },
    Png {
        hash: u64,
        data: Arc<Vec<u8>>,
    },
}

impl IconSource {
    pub fn pixmaps(data: Vec<IconPixmap>) -> IconSource {
        let mut h = DefaultHasher::new();
        for p in &data {
            (p.width, p.height, &p.pixels).hash(&mut h);
        }
        IconSource::Pixmaps { hash: h.finish(), data: Arc::new(data) }
    }

    pub fn png(data: Vec<u8>) -> IconSource {
        let mut h = DefaultHasher::new();
        data.hash(&mut h);
        IconSource::Png { hash: h.finish(), data: Arc::new(data) }
    }

    fn key(&self) -> u64 {
        let mut h = DefaultHasher::new();
        match self {
            IconSource::Builtin(b) => (0u8, b).hash(&mut h),
            IconSource::Named { name, theme_path } => (1u8, name, theme_path).hash(&mut h),
            IconSource::Pixmaps { hash, .. } => (2u8, hash).hash(&mut h),
            IconSource::Png { hash, .. } => (3u8, hash).hash(&mut h),
        }
        h.finish()
    }
}

/// Rasterized icons keyed by source and pixel size.
#[derive(Default)]
pub struct IconCache {
    cache: HashMap<(u64, u32), Option<Pixmap>>,
}

impl IconCache {
    /// Returns the first of `sources` that loads at `size`.
    pub fn first(&mut self, sources: &[IconSource], size: u32) -> Option<&Pixmap> {
        // Tray apps that animate their icon would otherwise grow this forever.
        if self.cache.len() > 512 {
            self.cache.clear();
        }
        let key = sources
            .iter()
            .map(|s| ((s.key(), size), s))
            .find_map(|(key, s)| self.cache.entry(key).or_insert_with(|| load(s, size)).is_some().then_some(key))?;
        self.cache.get(&key)?.as_ref()
    }
}

fn load(source: &IconSource, size: u32) -> Option<Pixmap> {
    let pixmap = match source {
        IconSource::Builtin(b) => render_svg(b.svg(), size),
        IconSource::Named { name, theme_path } => {
            find_icon(name, theme_path.as_deref(), size).and_then(|path| load_file(&path, size))
        }
        IconSource::Pixmaps { data, .. } => from_argb(data, size),
        IconSource::Png { data, .. } => Pixmap::decode_png(data).ok(),
    }?;
    Some(fit(pixmap, size))
}

fn render_svg(data: &[u8], size: u32) -> Option<Pixmap> {
    let tree = usvg::Tree::from_data(data, &usvg::Options::default()).ok()?;
    let mut pixmap = Pixmap::new(size, size)?;
    let s = tree.size();
    let scale = (size as f32 / s.width()).min(size as f32 / s.height());
    let dx = (size as f32 - s.width() * scale) / 2.0;
    let dy = (size as f32 - s.height() * scale) / 2.0;
    resvg::render(&tree, Transform::from_row(scale, 0.0, 0.0, scale, dx, dy), &mut pixmap.as_mut());
    Some(pixmap)
}

fn load_file(path: &Path, size: u32) -> Option<Pixmap> {
    let data = std::fs::read(path).ok()?;
    match path.extension().and_then(|e| e.to_str()) {
        Some("svg") | Some("svgz") => render_svg(&data, size),
        _ => Pixmap::decode_png(&data).ok(),
    }
}

fn find_icon(name: &str, theme_path: Option<&str>, size: u32) -> Option<PathBuf> {
    if name.starts_with('/') {
        return Some(PathBuf::from(name)).filter(|p| p.exists());
    }
    if let Some(dir) = theme_path.filter(|d| !d.is_empty())
        && let Some(p) = search_dir(Path::new(dir), name, 4)
    {
        return Some(p);
    }
    static THEME: OnceLock<Option<String>> = OnceLock::new();
    let theme = THEME.get_or_init(freedesktop_icons::default_theme_gtk);
    let size = size.min(u16::MAX as u32) as u16;
    let mut lookup = freedesktop_icons::lookup(name).with_size(size).with_cache();
    if let Some(theme) = theme {
        lookup = lookup.with_theme(theme);
    }
    lookup.find().or_else(|| {
        ["png", "svg", "xpm"]
            .iter()
            .map(|ext| PathBuf::from(format!("/usr/share/pixmaps/{name}.{ext}")))
            .find(|p| p.exists())
    })
}

/// Looks for `name.png`/`name.svg` under `dir`, preferring shallower matches.
fn search_dir(dir: &Path, name: &str, depth: u32) -> Option<PathBuf> {
    for ext in ["svg", "png"] {
        let p = dir.join(format!("{name}.{ext}"));
        if p.exists() {
            return Some(p);
        }
    }
    if depth == 0 {
        return None;
    }
    let mut dirs: Vec<_> =
        std::fs::read_dir(dir).ok()?.filter_map(Result::ok).map(|e| e.path()).filter(|p| p.is_dir()).collect();
    dirs.sort();
    dirs.iter().find_map(|d| search_dir(d, name, depth - 1))
}

/// Picks the smallest pixmap at least `size` wide (else the largest) and
/// converts it from ARGB32 in network byte order to premultiplied RGBA.
fn from_argb(pixmaps: &[IconPixmap], size: u32) -> Option<Pixmap> {
    // Dimensions come from other processes over D-Bus, so don't trust them.
    let area = |p: &IconPixmap| (p.width as usize).checked_mul(p.height as usize);
    let valid = pixmaps.iter().filter(|p| {
        p.width > 0 && p.height > 0 && area(p).and_then(|a| a.checked_mul(4)).is_some_and(|n| p.pixels.len() >= n)
    });
    let best = valid
        .clone()
        .filter(|p| p.width as u32 >= size)
        .min_by_key(|p| p.width)
        .or_else(|| valid.max_by_key(|p| p.width))?;
    let mut rgba = Vec::with_capacity(best.pixels.len());
    for &[a, r, g, b] in best.pixels.as_chunks::<4>().0.iter().take(area(best)?) {
        let pm = |c: u8| ((c as u16 * a as u16 + 127) / 255) as u8;
        rgba.extend_from_slice(&[pm(r), pm(g), pm(b), a]);
    }
    Pixmap::from_vec(rgba, IntSize::from_wh(best.width as u32, best.height as u32)?)
}

/// Scales `pixmap` to fit a `size`×`size` square, centered.
fn fit(pixmap: Pixmap, size: u32) -> Pixmap {
    if pixmap.width() == size && pixmap.height() == size {
        return pixmap;
    }
    let Some(mut out) = Pixmap::new(size, size) else { return pixmap };
    let scale = (size as f32 / pixmap.width() as f32).min(size as f32 / pixmap.height() as f32);
    let dx = (size as f32 - pixmap.width() as f32 * scale) / 2.0;
    let dy = (size as f32 - pixmap.height() as f32 * scale) / 2.0;
    let paint = PixmapPaint { quality: resvg::tiny_skia::FilterQuality::Bicubic, ..Default::default() };
    out.draw_pixmap(0, 0, pixmap.as_ref(), &paint, Transform::from_row(scale, 0.0, 0.0, scale, dx, dy), None);
    out
}
