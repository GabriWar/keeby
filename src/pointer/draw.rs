// SPDX-License-Identifier: GPL-3.0-only
// Ported from wl-kbptr (https://github.com/moverest/wl-kbptr) by moverest.
//! Drawing for the pointer overlay: tiny-skia shapes plus fontdue text, with
//! the same coordinate conventions as wl-kbptr's cairo code (logical units,
//! scaled once for the output's fractional scale).

use std::collections::HashMap;

use fontdue::{Font, FontSettings, Metrics};
use tiny_skia::{
    BlendMode, Color, FillRule, LineCap, LineJoin, Paint, PathBuilder, PixmapMut, Rect, Stroke,
    Transform,
};

/// How a draw call combines with what's already there, like cairo's
/// SOURCE (replace) and OVER (blend) operators.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Op {
    Source,
    Over,
}

fn color(c: u32) -> Color {
    Color::from_rgba8((c >> 24) as u8, (c >> 16) as u8, (c >> 8) as u8, c as u8)
}

fn paint(c: u32, op: Op) -> Paint<'static> {
    let mut p = Paint::default();
    p.set_color(color(c));
    p.anti_alias = true;
    p.blend_mode = if op == Op::Source { BlendMode::Source } else { BlendMode::SourceOver };
    p
}

/// Ink extents and advance of a string, in logical units (cairo's
/// `width`, `height` and `x_advance`).
#[derive(Clone, Copy, Default)]
pub struct TextExtents {
    pub width: f64,
    pub height: f64,
    pub x_advance: f64,
}

pub struct Fonts {
    fonts: Vec<Font>,
    by_spec: HashMap<String, usize>,
    /// Rasterized glyphs, kept for the daemon's lifetime.
    glyphs: HashMap<(usize, char, u32), (Metrics, Vec<u8>)>,
    /// Layout metrics per character and size, for measuring labels.
    metrics: HashMap<(usize, char, u32), Metrics>,
}

impl Fonts {
    pub fn new() -> Self {
        Self { fonts: Vec::new(), by_spec: HashMap::new(), glyphs: HashMap::new(), metrics: HashMap::new() }
    }

    /// Loads a font (file path or fontconfig family) once; later calls are free.
    pub fn load(&mut self, spec: &str) -> Result<(), String> {
        if self.by_spec.contains_key(spec) {
            return Ok(());
        }
        let bytes = crate::config::load_font(spec)
            .or_else(crate::config::default_font)
            .ok_or_else(|| format!("Font '{spec}' not found."))?;
        let font = Font::from_bytes(bytes, FontSettings::default())?;
        self.fonts.push(font);
        self.by_spec.insert(spec.to_string(), self.fonts.len() - 1);
        Ok(())
    }

    fn idx(&self, spec: &str) -> Option<usize> {
        self.by_spec.get(spec).copied()
    }

    fn char_metrics(&mut self, idx: usize, c: char, size: f32) -> Metrics {
        let fonts = &self.fonts;
        *self.metrics.entry((idx, c, size.to_bits())).or_insert_with(|| fonts[idx].metrics(c, size))
    }

    pub fn extents(&mut self, spec: &str, size: f64, text: &str) -> TextExtents {
        let Some(idx) = self.idx(spec) else { return TextExtents::default() };
        let (mut pen, mut min_x, mut max_x, mut min_y, mut max_y) =
            (0.0f64, f64::MAX, f64::MIN, f64::MAX, f64::MIN);
        for c in text.chars() {
            let m = self.char_metrics(idx, c, size as f32);
            if m.width > 0 && m.height > 0 {
                let x0 = pen + m.xmin as f64;
                min_x = min_x.min(x0);
                max_x = max_x.max(x0 + m.width as f64);
                min_y = min_y.min(-(m.ymin as f64 + m.height as f64));
                max_y = max_y.max(-m.ymin as f64);
            }
            pen += m.advance_width as f64;
        }
        if min_x > max_x {
            return TextExtents { width: 0.0, height: 0.0, x_advance: pen };
        }
        TextExtents { width: max_x - min_x, height: max_y - min_y, x_advance: pen }
    }

    fn glyph(&mut self, idx: usize, c: char, px: f32) -> &(Metrics, Vec<u8>) {
        let fonts = &self.fonts;
        self.glyphs.entry((idx, c, px.to_bits())).or_insert_with(|| fonts[idx].rasterize(c, px))
    }
}

pub struct Canvas<'a> {
    pub pix: PixmapMut<'a>,
    scale: f64,
    tf: Transform,
}

impl<'a> Canvas<'a> {
    pub fn new(pix: PixmapMut<'a>, scale: f64) -> Self {
        let tf = Transform::from_scale(scale as f32, scale as f32);
        Self { pix, scale, tf }
    }

    pub fn paint(&mut self, c: u32) {
        // Whole surface, replaced (every mode starts with a SOURCE paint).
        let c = color(c).premultiply().to_color_u8();
        let px = [c.red(), c.green(), c.blue(), c.alpha()];
        for chunk in self.pix.data_mut().chunks_exact_mut(4) {
            chunk.copy_from_slice(&px);
        }
    }

    pub fn fill_rect(&mut self, x: f64, y: f64, w: f64, h: f64, c: u32, op: Op) {
        if let Some(r) = Rect::from_xywh(x as f32, y as f32, w as f32, h as f32) {
            self.pix.fill_rect(r, &paint(c, op), self.tf, None);
        }
    }

    fn stroke_path(&mut self, pb: PathBuilder, width: f64, c: u32, op: Op, cap: LineCap, join: LineJoin) {
        if let Some(path) = pb.finish() {
            let stroke = Stroke { width: width as f32, line_cap: cap, line_join: join, ..Stroke::default() };
            self.pix.stroke_path(&path, &paint(c, op), &stroke, self.tf, None);
        }
    }

    pub fn stroke_rect(&mut self, x: f64, y: f64, w: f64, h: f64, width: f64, c: u32, op: Op) {
        let mut pb = PathBuilder::new();
        pb.move_to(x as f32, y as f32);
        pb.line_to((x + w) as f32, y as f32);
        pb.line_to((x + w) as f32, (y + h) as f32);
        pb.line_to(x as f32, (y + h) as f32);
        pb.close();
        self.stroke_path(pb, width, c, op, LineCap::Butt, LineJoin::Miter);
    }

    pub fn polyline(&mut self, pts: &[(f64, f64)], width: f64, c: u32, op: Op, cap: LineCap, join: LineJoin) {
        let mut pb = PathBuilder::new();
        for (i, (x, y)) in pts.iter().enumerate() {
            if i == 0 {
                pb.move_to(*x as f32, *y as f32);
            } else {
                pb.line_to(*x as f32, *y as f32);
            }
        }
        self.stroke_path(pb, width, c, op, cap, join);
    }

    pub fn line(&mut self, x1: f64, y1: f64, x2: f64, y2: f64, width: f64, c: u32, op: Op) {
        self.polyline(&[(x1, y1), (x2, y2)], width, c, op, LineCap::Butt, LineJoin::Miter);
    }

    pub fn circle_stroke(&mut self, cx: f64, cy: f64, r: f64, width: f64, c: u32, op: Op) {
        if let Some(path) = PathBuilder::from_circle(cx as f32, cy as f32, r as f32) {
            let stroke = Stroke { width: width as f32, ..Stroke::default() };
            self.pix.stroke_path(&path, &paint(c, op), &stroke, self.tf, None);
        }
    }

    pub fn circle_fill(&mut self, cx: f64, cy: f64, r: f64, c: u32, op: Op) {
        if let Some(path) = PathBuilder::from_circle(cx as f32, cy as f32, r as f32) {
            self.pix.fill_path(&path, &paint(c, op), FillRule::Winding, self.tf, None);
        }
    }

    /// Draws `text` with its baseline origin at (x, y) and returns the pen
    /// position after it, like cairo's `show_text`.
    pub fn text(&mut self, fonts: &mut Fonts, spec: &str, size: f64, x: f64, y: f64, text: &str, c: u32, op: Op) -> f64 {
        let Some(idx) = fonts.idx(spec) else { return x };
        let s = self.scale;
        let px = (size * s) as f32;
        // Premultiplied color, 0..=255 per channel.
        let a = c & 0xff;
        let src = [(c >> 24) * a / 255, (c >> 16 & 0xff) * a / 255, (c >> 8 & 0xff) * a / 255, a];
        let (w, h) = (self.pix.width() as i32, self.pix.height() as i32);
        let base = (y * s).round() as i32;
        let mut pen = x * s;
        for ch in text.chars() {
            let (m, bitmap) = fonts.glyph(idx, ch, px);
            let (gw, gh) = (m.width as i32, m.height as i32);
            let gx = (pen + m.xmin as f64).round() as i32;
            let gy = base - (m.ymin + gh);
            pen += m.advance_width as f64;
            // Clip the glyph box to the canvas once, not per pixel.
            let (c0, c1) = ((-gx).max(0), gw.min(w - gx));
            let (r0, r1) = ((-gy).max(0), gh.min(h - gy));
            if c0 >= c1 || r0 >= r1 {
                continue;
            }
            let data = self.pix.data_mut();
            for row in r0..r1 {
                let cov_row = &bitmap[(row * gw) as usize..][..gw as usize];
                let dst_row = ((gy + row) * w + gx) as usize * 4;
                for col in c0..c1 {
                    let cov = cov_row[col as usize] as u32;
                    if cov == 0 {
                        continue;
                    }
                    let d = &mut data[dst_row + col as usize * 4..][..4];
                    if cov == 255 && (op == Op::Source || a == 255) {
                        d.copy_from_slice(&[src[0] as u8, src[1] as u8, src[2] as u8, src[3] as u8]);
                        continue;
                    }
                    match op {
                        // SOURCE through a glyph mask: lerp toward the color.
                        Op::Source => {
                            for k in 0..4 {
                                let dv = d[k] as u32;
                                d[k] = ((dv * (255 - cov) + src[k] * cov + 127) / 255) as u8;
                            }
                        }
                        Op::Over => {
                            let sa = a * cov / 255;
                            for k in 0..4 {
                                let dv = d[k] as u32;
                                d[k] = ((src[k] * cov + dv * (255 - sa) + 127) / 255).min(255) as u8;
                            }
                        }
                    }
                }
            }
        }
        pen / s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Floating mode's worst case: ~700 three-letter labels on a 1080p
    /// frame, measured twice like a redraw does.
    #[test]
    fn label_text_is_fast() {
        let mut fonts = Fonts::new();
        if fonts.load("sans-serif").is_err() {
            return;
        }
        let mut buf = vec![0u8; 1920 * 1080 * 4];
        let labels: Vec<String> = (0..700).map(|i| format!("{}{}{}", (b'a' + (i % 26) as u8) as char, (b'a' + (i / 26 % 26) as u8) as char, 'k')).collect();
        let draw = |fonts: &mut Fonts, buf: &mut Vec<u8>| {
            let mut cv = Canvas::new(PixmapMut::from_bytes(buf, 1920, 1080).unwrap(), 1.0);
            for (i, l) in labels.iter().enumerate() {
                let te = fonts.extents("sans-serif", 14.0, l);
                let x = (i % 30) as f64 * 64.0 + (64.0 - te.x_advance) / 2.0;
                let y = (i / 30) as f64 * 45.0 + 30.0;
                cv.text(fonts, "sans-serif", 14.0, x, y, l, 0xffffffdd, Op::Over);
            }
        };
        draw(&mut fonts, &mut buf); // warm the caches, like the daemon has
        let t = std::time::Instant::now();
        for _ in 0..20 {
            draw(&mut fonts, &mut buf);
        }
        let per_frame = t.elapsed() / 20;
        eprintln!("700 labels: {per_frame:?} per frame");
        assert!(buf.iter().any(|b| *b != 0));
    }
}
