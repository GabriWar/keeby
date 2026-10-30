//! Live preview for `keeby config`: draws the overlay or a pointer mode with
//! the real renderers, over a screenshot taken when the page opened.

use evdev::Key;
use tiny_skia::{Pixmap, PixmapPaint, Transform};

use crate::config::{Anchor, Config};
use crate::input::MouseBtn;
use crate::mouse::{MouseRenderer, MouseState};
use crate::pointer::{preview, Rect};
use crate::render::{Renderer, State as KeyState, HEIGHT, WIDTH};

pub struct Preview {
    /// The screenshot as PNG, served as is for the page's background.
    pub screen_png: Vec<u8>,
    w: u32,
    h: u32,
    targets: Vec<Rect>,
}

/// The output to show: `monitor` from the config, else the focused one.
fn output_name(cfg: &Config) -> Option<String> {
    if let Some(m) = cfg.monitor.as_ref().filter(|m| !m.eq_ignore_ascii_case("auto")) {
        return Some(m.clone());
    }
    let out = std::process::Command::new("hyprctl").args(["-j", "monitors"]).output().ok()?;
    let json = String::from_utf8_lossy(&out.stdout);
    // Each monitor object starts with its id; keep the one marked focused.
    json.split("\"id\":").find(|m| m.contains("\"focused\": true")).and_then(|m| {
        let rest = &m[m.find("\"name\": \"")? + 9..];
        Some(rest[..rest.find('"')?].to_string())
    })
}

/// A made-up desktop: a bar and a window full of buttons, so nothing private
/// ends up on the page unless asked for.
fn mock_desktop() -> Pixmap {
    use tiny_skia::{Color, Paint, Rect as SkRect};
    let mut p = Pixmap::new(1920, 1080).unwrap();
    p.fill(Color::from_rgba8(0x2b, 0x30, 0x3b, 0xff));
    let fill = |p: &mut Pixmap, x: f32, y: f32, w: f32, h: f32, c: (u8, u8, u8)| {
        let mut paint = Paint::default();
        paint.set_color_rgba8(c.0, c.1, c.2, 0xff);
        if let Some(r) = SkRect::from_xywh(x, y, w, h) {
            p.fill_rect(r, &paint, Transform::identity(), None);
        }
    };
    fill(&mut p, 0.0, 0.0, 1920.0, 34.0, (0x1b, 0x1e, 0x25));
    for i in 0..9 {
        fill(&mut p, 12.0 + i as f32 * 38.0, 7.0, 26.0, 20.0, (0x55, 0x5c, 0x6c));
    }
    for i in 0..5 {
        fill(&mut p, 1600.0 + i as f32 * 60.0, 9.0, 44.0, 16.0, (0x55, 0x5c, 0x6c));
    }
    fill(&mut p, 160.0, 90.0, 1600.0, 900.0, (0xe9, 0xeb, 0xef));
    fill(&mut p, 160.0, 90.0, 1600.0, 46.0, (0xd0, 0xd4, 0xdb));
    for i in 0..8 {
        fill(&mut p, 180.0 + i as f32 * 110.0, 100.0, 92.0, 26.0, (0xf7, 0xf8, 0xfa));
    }
    fill(&mut p, 160.0, 136.0, 260.0, 854.0, (0xdf, 0xe2, 0xe8));
    for i in 0..14 {
        fill(&mut p, 180.0, 160.0 + i as f32 * 56.0, 200.0 + (i % 3) as f32 * 10.0 - 10.0, 30.0, (0xc3, 0xc8, 0xd2));
    }
    for r in 0..12 {
        for c in 0..6 {
            let (x, y) = (460.0 + c as f32 * 210.0, 170.0 + r as f32 * 66.0);
            fill(&mut p, x, y, 60.0 + ((r * 7 + c * 3) % 5) as f32 * 26.0, 34.0, (0x5c + (c * 12) as u8, 0x7a, 0xc8 - (r * 6) as u8));
        }
    }
    p
}

impl Preview {
    pub fn new() -> Self {
        let mut pix = mock_desktop();
        let png = pix.encode_png().unwrap_or_default();
        Self::from_pixmap(png, &mut pix)
    }

    /// Replaces the background with a screenshot of the screen.
    pub fn screenshot(&mut self, cfg: &Config) -> Result<(), String> {
        let mut cmd = std::process::Command::new("grim");
        if let Some(o) = output_name(cfg) {
            cmd.args(["-o", &o]);
        }
        let out = cmd.args(["-t", "png", "-l", "1", "-"]).output().map_err(|e| format!("grim: {e}"))?;
        if !out.status.success() {
            return Err(String::from_utf8_lossy(&out.stderr).into_owned());
        }
        let mut pix = Pixmap::decode_png(&out.stdout).map_err(|e| e.to_string())?;
        *self = Self::from_pixmap(out.stdout, &mut pix);
        Ok(())
    }

    fn from_pixmap(screen_png: Vec<u8>, pix: &mut Pixmap) -> Self {
        let (w, h) = (pix.width(), pix.height());
        let targets = preview::targets(pix.data_mut(), w, h);
        Self { screen_png, w, h, targets }
    }

    /// A transparent PNG the size of the screen with `view` drawn on it:
    /// `overlay` (key pills and mouse widget) or a pointer mode.
    pub fn render(&self, config_text: &str, view: &str) -> Result<Vec<u8>, String> {
        let cfg = Config::parse(config_text);
        let mut out = Pixmap::new(self.w, self.h).ok_or("bad screen size")?;
        if view == "overlay" {
            self.overlay(&cfg, &mut out);
        } else {
            preview::render(&cfg.theme, &cfg.pointer_settings, view, &self.targets, out.as_mut())?;
        }
        out.encode_png().map_err(|e| e.to_string())
    }

    fn overlay(&self, cfg: &Config, out: &mut Pixmap) {
        let mut keys = KeyState::new(cfg);
        for k in [Key::KEY_K, Key::KEY_E, Key::KEY_E, Key::KEY_B, Key::KEY_Y, Key::KEY_SPACE] {
            keys.press(k);
        }
        keys.press(Key::KEY_LEFTCTRL);
        keys.press(Key::KEY_LEFTSHIFT);
        keys.press(Key::KEY_T);
        let mut pills = Pixmap::new(WIDTH, HEIGHT).unwrap();
        Renderer::new(cfg).render(&mut pills.as_mut(), &mut keys);
        let at = place(cfg.keys_anchor, cfg.keys_margin_x, cfg.margin_bottom, (WIDTH, HEIGHT), (self.w, self.h));
        out.draw_pixmap(at.0, at.1, pills.as_ref(), &PixmapPaint::default(), Transform::identity(), None);

        if cfg.mouse_enabled {
            let r = MouseRenderer::new(cfg);
            let (mw, mh) = r.surface_size();
            let mut st = MouseState::new();
            st.add_motion(300, -200);
            st.add_scroll(1, 0);
            st.set_button(MouseBtn::Left, true);
            let mut m = Pixmap::new(mw, mh).unwrap();
            r.render(&mut m.as_mut(), &st);
            let at = place(cfg.mouse_anchor, cfg.mouse_margin_x, cfg.mouse_margin_y, (mw, mh), (self.w, self.h));
            out.draw_pixmap(at.0, at.1, m.as_ref(), &PixmapPaint::default(), Transform::identity(), None);
        }
    }
}

/// Where the compositor puts a layer surface of `size` with this anchor and margins.
fn place(a: Anchor, mx: i32, my: i32, (sw, sh): (u32, u32), (w, h): (u32, u32)) -> (i32, i32) {
    let (sw, sh, w, h) = (sw as i32, sh as i32, w as i32, h as i32);
    use Anchor::*;
    let x = match a {
        TopLeft | Left | BottomLeft => mx,
        TopRight | Right | BottomRight => w - sw - mx,
        _ => (w - sw) / 2,
    };
    let y = match a {
        TopLeft | Top | TopRight => my,
        BottomLeft | Bottom | BottomRight => h - sh - my,
        _ => (h - sh) / 2,
    };
    (x, y)
}
