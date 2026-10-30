//! Draws a pointer mode the way a run would show it, for the settings page.

use tiny_skia::PixmapMut;
use xkbcommon::xkb::Keysym;

use super::config::{FloatingSource, PointerConfig};
use super::draw::{Canvas, Fonts};
use super::modes::{AreaSource, Engine, Rect};

/// Targets found on the screenshot, handed to floating mode whatever its source.
struct Targets<'a>(&'a [Rect]);

impl AreaSource for Targets<'_> {
    fn areas(&mut self, _: FloatingSource, _: Rect) -> Vec<Rect> {
        self.0.to_vec()
    }
}

/// Finds targets on an RGBA screenshot, or makes up a few when there are none.
pub fn targets(rgba: &mut [u8], w: u32, h: u32) -> Vec<Rect> {
    const ABGR8888: u32 = 0x34324241;
    let area = Rect { x: 0, y: 0, w: w as i32, h: h as i32 };
    let found = super::detect::detect(rgba, w, h, w * 4, ABGR8888, 0, area);
    if !found.is_empty() {
        return found;
    }
    (0..60)
        .map(|i| Rect { x: 60 + (i % 10) * (w as i32 - 120) / 10, y: 80 + (i / 10) * (h as i32 - 160) / 6, w: 24 + (i % 3) * 40, h: 18 + (i % 2) * 14 })
        .collect()
}

/// Renders `view` (floating, tile, bisect, split or drag) with a key or two
/// already pressed so the typed and history colors show too.
pub fn render(theme: &crate::config::Theme, settings: &[(String, String)], view: &str, targets: &[Rect], pix: PixmapMut) -> Result<(), String> {
    let mut cfg = PointerConfig::new(theme);
    for (k, v) in settings {
        cfg.set(k, v)?;
    }
    let drag = view == "drag";
    cfg.general.modes = if drag { "split".into() } else { view.into() };
    let screen = Rect { x: 0, y: 0, w: pix.width() as i32, h: pix.height() as i32 };
    let home_row = "asdfjklmghb".chars().map(|c| c.to_string()).collect();
    let mut engine = Engine::new(cfg, home_row, drag, false)?;
    let mut fonts = Fonts::new();
    for spec in engine.font_specs() {
        fonts.load(&spec)?;
    }
    let mut src = Targets(targets);
    engine.enter_next_mode(screen, &mut src);
    let keys: &[(Keysym, &str)] = match view {
        "bisect" => &[(Keysym::d, "d")],
        "split" => &[(Keysym::Left, "")],
        "drag" => &[(Keysym::Left, ""), (Keysym::Return, "\r"), (Keysym::Down, "")],
        _ => &[],
    };
    for (sym, text) in keys {
        engine.key(*sym, text, &mut src);
    }
    let mut cv = Canvas::new(pix, 1.0);
    engine.render(&mut cv, &mut fonts);
    Ok(())
}
