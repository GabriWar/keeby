use std::collections::{HashSet, VecDeque};
use std::time::Instant;

use evdev::Key;
use fontdue::{Font, FontSettings, Metrics};
use tiny_skia::{
    Color, FillRule, Paint, PathBuilder, Pixmap, PremultipliedColorU8, Transform,
};

use crate::config::{Config, Rgba};
use crate::labels::{is_modifier, label, modifier_order};

pub const WIDTH: u32 = 1000;
pub const HEIGHT: u32 = 90;

const PILL_GAP: f32 = 12.0;
const MARGIN_X: f32 = 20.0;
const MAX_PILLS: usize = 32;

pub struct Pill {
    pub text: String,
    pub created: Instant,
}

pub struct State {
    held: HashSet<Key>,
    pub stream: VecDeque<Pill>,
    pub slide_offset: f32,
    pub last_anim: Instant,
    pub life_secs: f32,
    pub fade_secs: f32,
}

impl State {
    pub fn new(cfg: &Config) -> Self {
        Self {
            held: HashSet::new(),
            stream: VecDeque::new(),
            slide_offset: 0.0,
            last_anim: Instant::now(),
            life_secs: cfg.pill_life_secs,
            fade_secs: cfg.pill_fade_secs,
        }
    }

    /// Push a synthetic labeled pill (e.g. "ScrollUp"). Dedups against the most
    /// recent identical pill within `dedup_ms` so fast scroll wheels don't flood.
    pub fn push_label(&mut self, text: &str, dedup_ms: u128) -> bool {
        let now = Instant::now();
        if let Some(last) = self.stream.back() {
            if last.text == text && now.duration_since(last.created).as_millis() < dedup_ms {
                return false;
            }
        }
        self.stream.push_back(Pill {
            text: text.to_string(),
            created: now,
        });
        while self.stream.len() > MAX_PILLS {
            self.stream.pop_front();
        }
        true
    }

    /// Returns `true` if a pill was pushed (non-modifier press).
    pub fn press(&mut self, k: Key) -> bool {
        if is_modifier(k) {
            self.held.insert(k);
            return false;
        }
        let mut mods: Vec<Key> = self.held.iter().copied().collect();
        mods.sort_by_key(|m| modifier_order(*m));
        mods.dedup_by_key(|m| modifier_order(*m));
        let mut text = String::new();
        for m in &mods {
            text.push_str(label(*m));
            text.push('+');
        }
        text.push_str(label(k));
        self.stream.push_back(Pill {
            text,
            created: Instant::now(),
        });
        while self.stream.len() > MAX_PILLS {
            self.stream.pop_front();
        }
        true
    }

    pub fn release(&mut self, k: Key) {
        if is_modifier(k) {
            self.held.remove(&k);
        }
    }

    pub fn prune(&mut self) -> bool {
        let before = self.stream.len();
        let now = Instant::now();
        let life = self.life_secs;
        self.stream
            .retain(|p| now.duration_since(p.created).as_secs_f32() < life);
        before != self.stream.len()
    }

    pub fn step_animation(&mut self) {
        // No slide animation — pills snap into place. Only time-fade.
        self.slide_offset = 0.0;
    }

    pub fn has_animation(&self) -> bool {
        // Animate only while the oldest pill is in its fade window.
        let now = Instant::now();
        self.stream
            .iter()
            .any(|p| now.duration_since(p.created).as_secs_f32() > self.life_secs - self.fade_secs)
    }
}

pub struct Renderer {
    font: Font,
    glyph_cache: std::collections::HashMap<char, (Metrics, Vec<u8>)>,
    font_size: f32,
    pill_h: f32,
    pill_pad_x: f32,
    bg_color: Rgba,
    accent_color: Rgba,
    text_color: Rgba,
    text_dim_color: Rgba,
}

impl Renderer {
    pub fn new(cfg: &Config) -> Self {
        let bytes = cfg.resolve_font_bytes();
        let font = Font::from_bytes(bytes, FontSettings::default()).expect("parse font");
        let font_size = cfg.font_size;
        Self {
            font,
            glyph_cache: std::collections::HashMap::new(),
            font_size,
            pill_h: (font_size * 2.4).max(48.0),
            pill_pad_x: font_size * 0.85,
            bg_color: cfg.bg_color,
            accent_color: cfg.accent_color,
            text_color: cfg.text_color,
            text_dim_color: cfg.text_dim_color,
        }
    }

    pub fn pill_width(&mut self, text: &str) -> f32 {
        let text_w = self.measure(text).max(self.font_size * 0.8);
        text_w + 2.0 * self.pill_pad_x
    }

    fn measure(&mut self, text: &str) -> f32 {
        let mut w = 0.0;
        for ch in text.chars() {
            let (m, _) = self
                .glyph_cache
                .entry(ch)
                .or_insert_with(|| self.font.rasterize(ch, self.font_size));
            w += m.advance_width;
        }
        w
    }

    fn draw_text(
        &mut self,
        pixmap: &mut Pixmap,
        x: f32,
        y_baseline: f32,
        text: &str,
        rgb: (u8, u8, u8),
        alpha: f32,
    ) {
        let mut pen_x = x;
        for ch in text.chars() {
            let (metrics, bitmap) = self
                .glyph_cache
                .entry(ch)
                .or_insert_with(|| self.font.rasterize(ch, self.font_size));
            let gx0 = (pen_x + metrics.xmin as f32).round() as i32;
            let gy0 = (y_baseline - metrics.height as f32 - metrics.ymin as f32).round() as i32;
            blit_glyph(
                pixmap,
                gx0,
                gy0,
                metrics.width,
                metrics.height,
                bitmap,
                rgb,
                alpha,
            );
            pen_x += metrics.advance_width;
        }
    }

    pub fn render(&mut self, pixmap: &mut Pixmap, state: &mut State) {
        pixmap.fill(Color::TRANSPARENT);
        if state.stream.is_empty() {
            return;
        }

        let pill_y = (HEIGHT as f32 - self.pill_h) / 2.0;
        let right_edge = WIDTH as f32 - MARGIN_X;
        let now = Instant::now();
        let life = state.life_secs;
        let fade = state.fade_secs;

        // Pre-compute widths.
        let widths: Vec<f32> = state
            .stream
            .iter()
            .map(|p| {
                let text_w = self.measure(&p.text).max(self.font_size * 0.8);
                text_w + 2.0 * self.pill_pad_x
            })
            .collect();

        // Place right-to-left starting at right_edge.
        let mut x_right = right_edge;
        let n = state.stream.len();
        for i in (0..n).rev() {
            let pill = &state.stream[i];
            let pw = widths[i];
            let x = x_right - pw;
            let age = now.duration_since(pill.created).as_secs_f32();
            let alpha = if age < life - fade {
                1.0
            } else {
                ((life - age) / fade).clamp(0.0, 1.0)
            };
            // Newest gets a brighter accent.
            let is_newest = i == n - 1;
            self.draw_pill(pixmap, x, pill_y, pw, &pill.text, alpha, is_newest);
            x_right = x - PILL_GAP;
            if x_right < -50.0 {
                break; // off-screen left, stop drawing
            }
        }
    }

    fn draw_pill(
        &mut self,
        pixmap: &mut Pixmap,
        x: f32,
        y: f32,
        pw: f32,
        text: &str,
        alpha: f32,
        is_newest: bool,
    ) {
        let pill_h = self.pill_h;
        let radius = pill_h * 0.5;

        // Solid pill bg (no gradient, no shadow — fast).
        let mut path = PathBuilder::new();
        rounded_rect(&mut path, x, y, pw, pill_h, radius);
        let path = path.finish().unwrap();

        let bg_src = if is_newest { self.accent_color } else { self.bg_color };
        let bg = Color::from_rgba(bg_src.0, bg_src.1, bg_src.2, bg_src.3 * alpha)
            .unwrap_or(Color::TRANSPARENT);

        // Subtle border using accent at low alpha — keeps pills readable on any bg.
        let border_src = self.accent_color;
        let border_color = Color::from_rgba(
            border_src.0,
            border_src.1,
            border_src.2,
            0.6 * alpha,
        )
        .unwrap_or(Color::TRANSPARENT);

        let mut bg_paint = Paint::default();
        bg_paint.set_color(bg);
        bg_paint.anti_alias = true;
        pixmap.fill_path(
            &path,
            &bg_paint,
            FillRule::Winding,
            Transform::identity(),
            None,
        );

        // Border (1.5px stroke).
        let mut border_paint = Paint::default();
        border_paint.set_color(border_color);
        border_paint.anti_alias = true;
        let stroke = tiny_skia::Stroke {
            width: 1.5,
            ..Default::default()
        };
        pixmap.stroke_path(&path, &border_paint, &stroke, Transform::identity(), None);

        // Text.
        let text_w = self.measure(text);
        let baseline = y + pill_h / 2.0 + self.font_size * 0.34;
        let text_x = x + (pw - text_w) / 2.0;
        let tc = if is_newest { self.text_color } else { self.text_dim_color };
        let text_rgb = (
            (tc.0 * 255.0) as u8,
            (tc.1 * 255.0) as u8,
            (tc.2 * 255.0) as u8,
        );
        self.draw_text(pixmap, text_x, baseline, text, text_rgb, alpha * tc.3);
    }
}

fn rounded_rect(p: &mut PathBuilder, x: f32, y: f32, w: f32, h: f32, r: f32) {
    let r = r.min(w / 2.0).min(h / 2.0);
    p.move_to(x + r, y);
    p.line_to(x + w - r, y);
    p.quad_to(x + w, y, x + w, y + r);
    p.line_to(x + w, y + h - r);
    p.quad_to(x + w, y + h, x + w - r, y + h);
    p.line_to(x + r, y + h);
    p.quad_to(x, y + h, x, y + h - r);
    p.line_to(x, y + r);
    p.quad_to(x, y, x + r, y);
    p.close();
}

fn blit_glyph(
    pixmap: &mut Pixmap,
    x0: i32,
    y0: i32,
    gw: usize,
    gh: usize,
    bitmap: &[u8],
    rgb: (u8, u8, u8),
    alpha_mul: f32,
) {
    let pw = pixmap.width() as i32;
    let ph = pixmap.height() as i32;
    let stride = pixmap.width() as usize;
    let pixels = pixmap.pixels_mut();
    let (sr, sg, sb) = (rgb.0 as u32, rgb.1 as u32, rgb.2 as u32);
    let alpha_u = (alpha_mul.clamp(0.0, 1.0) * 255.0) as u32;
    if alpha_u == 0 {
        return;
    }

    // Clip Y range up-front.
    let y_start = (-y0).max(0);
    let y_end = ((ph - y0).min(gh as i32)).max(0);
    let x_start = (-x0).max(0);
    let x_end = ((pw - x0).min(gw as i32)).max(0);

    for gy in y_start..y_end {
        let py = (y0 + gy) as usize;
        let row_start = (gy as usize) * gw;
        let dst_row = py * stride;
        for gx in x_start..x_end {
            let cov_raw = bitmap[row_start + gx as usize] as u32;
            if cov_raw == 0 {
                continue;
            }
            // cov = cov_raw * alpha_mul, in 0..=255
            let cov = (cov_raw * alpha_u + 127) / 255;
            if cov == 0 {
                continue;
            }
            let inv = 255 - cov;
            let didx = dst_row + (x0 + gx) as usize;
            let dst = pixels[didx];
            // Premul source.
            let src_r = (sr * cov + 127) / 255;
            let src_g = (sg * cov + 127) / 255;
            let src_b = (sb * cov + 127) / 255;
            let dr = dst.red() as u32;
            let dg = dst.green() as u32;
            let db = dst.blue() as u32;
            let da = dst.alpha() as u32;
            let or = (src_r + (dr * inv + 127) / 255).min(255) as u8;
            let og = (src_g + (dg * inv + 127) / 255).min(255) as u8;
            let ob = (src_b + (db * inv + 127) / 255).min(255) as u8;
            let oa = (cov + (da * inv + 127) / 255).min(255) as u8;
            pixels[didx] = PremultipliedColorU8::from_rgba(or, og, ob, oa)
                .unwrap_or(PremultipliedColorU8::TRANSPARENT);
        }
    }
}

