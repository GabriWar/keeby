use std::time::Instant;

use tiny_skia::{Color, FillRule, Paint, PathBuilder, Pixmap, Stroke, Transform};

use crate::config::{Config, Orientation, Rgba};
use crate::input::MouseBtn;

const MOTION_DECAY_PER_SEC: f32 = 6.0; // higher = faster decay
const SCROLL_DECAY_PER_SEC: f32 = 5.0;
const MOTION_VIS_SCALE: f32 = 0.06; // px input per 1.0 visual unit

pub struct MouseState {
    pub motion: (f32, f32),
    pub scroll_v: f32,
    pub scroll_h: f32,
    pub btn_left: bool,
    pub btn_middle: bool,
    pub btn_right: bool,
    pub btn_side: bool,
    pub btn_extra: bool,
    pub last_step: Instant,
}

impl MouseState {
    pub fn new() -> Self {
        Self {
            motion: (0.0, 0.0),
            scroll_v: 0.0,
            scroll_h: 0.0,
            btn_left: false,
            btn_middle: false,
            btn_right: false,
            btn_side: false,
            btn_extra: false,
            last_step: Instant::now(),
        }
    }

    pub fn add_motion(&mut self, dx: i32, dy: i32) {
        self.motion.0 += dx as f32 * MOTION_VIS_SCALE;
        self.motion.1 += dy as f32 * MOTION_VIS_SCALE;
    }

    pub fn add_scroll(&mut self, v: i32, h: i32) {
        self.scroll_v += v as f32;
        self.scroll_h += h as f32;
    }

    pub fn set_button(&mut self, b: MouseBtn, pressed: bool) {
        match b {
            MouseBtn::Left => self.btn_left = pressed,
            MouseBtn::Middle => self.btn_middle = pressed,
            MouseBtn::Right => self.btn_right = pressed,
            MouseBtn::Side => self.btn_side = pressed,
            MouseBtn::Extra => self.btn_extra = pressed,
        }
    }

    pub fn step(&mut self) {
        let now = Instant::now();
        let dt = now.duration_since(self.last_step).as_secs_f32().min(0.1);
        self.last_step = now;
        let m_factor = (-MOTION_DECAY_PER_SEC * dt).exp();
        self.motion.0 *= m_factor;
        self.motion.1 *= m_factor;
        if self.motion.0.abs() < 0.05 {
            self.motion.0 = 0.0;
        }
        if self.motion.1.abs() < 0.05 {
            self.motion.1 = 0.0;
        }
        let s_factor = (-SCROLL_DECAY_PER_SEC * dt).exp();
        self.scroll_v *= s_factor;
        self.scroll_h *= s_factor;
        if self.scroll_v.abs() < 0.02 {
            self.scroll_v = 0.0;
        }
        if self.scroll_h.abs() < 0.02 {
            self.scroll_h = 0.0;
        }
    }

    pub fn has_animation(&self) -> bool {
        self.motion.0.abs() > 0.01
            || self.motion.1.abs() > 0.01
            || self.scroll_v.abs() > 0.01
            || self.scroll_h.abs() > 0.01
    }
}

pub struct MouseRenderer {
    size: f32,
    orientation: Orientation,
    show_arrow: bool,
    show_buttons: bool,
    show_scroll: bool,
    bg: Rgba,
    accent: Rgba,
    arrow: Rgba,
    text: Rgba,
}

impl MouseRenderer {
    pub fn new(cfg: &Config) -> Self {
        Self {
            size: cfg.mouse_size as f32,
            orientation: cfg.mouse_orientation,
            show_arrow: cfg.mouse_show_arrow,
            show_buttons: cfg.mouse_show_buttons,
            show_scroll: cfg.mouse_show_scroll,
            bg: Rgba::from(cfg.theme.bg),
            accent: Rgba::from(cfg.theme.accent),
            arrow: cfg.mouse_arrow(),
            text: Rgba::from(cfg.theme.text),
        }
    }

    /// Surface dimensions for this widget (px).
    pub fn surface_size(&self) -> (u32, u32) {
        let s = self.size;
        // Long axis fits cursor + button cluster.
        // Short axis is taller than `s` to leave room for the compass arrows
        // that sit above/below the cursor disc.
        let long = s * 2.6;
        let short = s * 1.4;
        match self.orientation {
            Orientation::Horizontal => (long.ceil() as u32, short.ceil() as u32),
            Orientation::Vertical => (short.ceil() as u32, long.ceil() as u32),
        }
    }

    pub fn render(&self, pixmap: &mut Pixmap, state: &MouseState) {
        pixmap.fill(Color::TRANSPARENT);
        let (w, h) = (pixmap.width() as f32, pixmap.height() as f32);
        let s = self.size;
        let r = s * 0.42;

        // Cursor sits inset from the leading edge so left/up compass arrows
        // have room to extend past the disc without clipping.
        let (cx, cy, btn_cx, btn_cy) = match self.orientation {
            Orientation::Horizontal => (s * 0.65, h * 0.5, s * 0.65 + s * 1.1, h * 0.5),
            Orientation::Vertical => (w * 0.5, s * 0.65, w * 0.5, s * 0.65 + s * 1.1),
        };

        // Cursor disc + ring (ring drawn thick & bright so widget stays visible
        // even when bg is fully transparent).
        circle(pixmap, cx, cy, r, rgba(self.bg));
        ring(pixmap, cx, cy, r, 2.5, rgba(self.accent.scale_alpha(0.95)));

        // Motion arrow / idle dot.
        if self.show_arrow {
            let (dx, dy) = state.motion;
            let mag = (dx * dx + dy * dy).sqrt();
            if mag > 0.05 {
                let len = mag.min(1.0) * (r * 0.85);
                let nx = dx / mag.max(1e-6);
                let ny = dy / mag.max(1e-6);
                draw_arrow(
                    pixmap,
                    (cx, cy),
                    (cx + nx * len, cy + ny * len),
                    s * 0.10,
                    rgba(self.arrow),
                );
            } else {
                circle(pixmap, cx, cy, s * 0.06, rgba(self.text.scale_alpha(0.65)));
            }
        }

        // Compass arrows around the cursor — always visible at a dim base
        // alpha, brighten when scroll fires in their direction.
        if self.show_scroll {
            let sv = state.scroll_v;
            let sh_ = state.scroll_h;
            let base = 0.28;
            let arrow_size = s * 0.10;
            let dist = r + s * 0.07;
            // Up: brightens with scroll_v > 0
            let up_a = base + (sv.max(0.0).min(2.0) / 2.0) * (1.0 - base);
            compass_arrow(pixmap, cx, cy - dist, arrow_size, Dir::Up, rgba(self.arrow.scale_alpha(up_a)));
            // Down: brightens with scroll_v < 0
            let down_a = base + ((-sv).max(0.0).min(2.0) / 2.0) * (1.0 - base);
            compass_arrow(pixmap, cx, cy + dist, arrow_size, Dir::Down, rgba(self.arrow.scale_alpha(down_a)));
            // Right: brightens with scroll_h > 0
            let right_a = base + (sh_.max(0.0).min(2.0) / 2.0) * (1.0 - base);
            compass_arrow(pixmap, cx + dist, cy, arrow_size, Dir::Right, rgba(self.arrow.scale_alpha(right_a)));
            // Left: brightens with scroll_h < 0
            let left_a = base + ((-sh_).max(0.0).min(2.0) / 2.0) * (1.0 - base);
            compass_arrow(pixmap, cx - dist, cy, arrow_size, Dir::Left, rgba(self.arrow.scale_alpha(left_a)));
        }

        // Three buttons (LMB / MMB / RMB).
        if self.show_buttons {
            let buttons = [state.btn_left, state.btn_middle, state.btn_right];
            // Squatter buttons — were s*0.55 tall, looked like pills standing on end.
            let (long, short) = (s * 0.35, s * 0.28);
            let gap = s * 0.08;
            match self.orientation {
                Orientation::Horizontal => {
                    let total = short * 3.0 + gap * 2.0;
                    let mut x = btn_cx - total / 2.0;
                    let y = btn_cy - long / 2.0;
                    for pressed in buttons {
                        button_rect(pixmap, x, y, short, long, pressed, self.bg, self.accent);
                        x += short + gap;
                    }
                }
                Orientation::Vertical => {
                    let total = short * 3.0 + gap * 2.0;
                    let x = btn_cx - long / 2.0;
                    let mut y = btn_cy - total / 2.0;
                    for pressed in buttons {
                        button_rect(pixmap, x, y, long, short, pressed, self.bg, self.accent);
                        y += short + gap;
                    }
                }
            }
        }
    }
}

fn button_rect(
    pixmap: &mut Pixmap,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    pressed: bool,
    bg: Rgba,
    accent: Rgba,
) {
    let bgc = if pressed { rgba(accent) } else { rgba(bg.scale_alpha(0.85)) };
    let mut path = PathBuilder::new();
    rounded_rect(&mut path, x, y, w, h, w.min(h) * 0.32);
    let path = path.finish().unwrap();
    let mut paint = Paint::default();
    paint.set_color(bgc);
    paint.anti_alias = true;
    pixmap.fill_path(&path, &paint, FillRule::Winding, Transform::identity(), None);
}

fn rgba(c: Rgba) -> Color {
    Color::from_rgba(c.0, c.1, c.2, c.3).unwrap_or(Color::TRANSPARENT)
}

fn circle(pixmap: &mut Pixmap, cx: f32, cy: f32, r: f32, color: Color) {
    let mut p = PathBuilder::new();
    p.push_circle(cx, cy, r);
    let path = p.finish().unwrap();
    let mut paint = Paint::default();
    paint.set_color(color);
    paint.anti_alias = true;
    pixmap.fill_path(&path, &paint, FillRule::Winding, Transform::identity(), None);
}

fn ring(pixmap: &mut Pixmap, cx: f32, cy: f32, r: f32, w: f32, color: Color) {
    let mut p = PathBuilder::new();
    p.push_circle(cx, cy, r);
    let path = p.finish().unwrap();
    let mut paint = Paint::default();
    paint.set_color(color);
    paint.anti_alias = true;
    let stroke = Stroke {
        width: w,
        ..Default::default()
    };
    pixmap.stroke_path(&path, &paint, &stroke, Transform::identity(), None);
}

/// Single filled isosceles triangle: base centered at `from`, apex at `to`.
/// Width at base scales with length (capped) so it looks like a proper arrow blade.
fn draw_arrow(pixmap: &mut Pixmap, from: (f32, f32), to: (f32, f32), thickness: f32, color: Color) {
    let dx = to.0 - from.0;
    let dy = to.1 - from.1;
    let len = (dx * dx + dy * dy).sqrt();
    if len < 1e-3 {
        return;
    }
    let ux = dx / len;
    let uy = dy / len;
    let px = -uy; // perpendicular
    let py = ux;
    let half_w = (thickness * 0.85).min(len * 0.35);
    let bx0 = from.0 + px * half_w;
    let by0 = from.1 + py * half_w;
    let bx1 = from.0 - px * half_w;
    let by1 = from.1 - py * half_w;
    let mut p = PathBuilder::new();
    p.move_to(to.0, to.1);
    p.line_to(bx0, by0);
    p.line_to(bx1, by1);
    p.close();
    let path = p.finish().unwrap();
    let mut paint = Paint::default();
    paint.set_color(color);
    paint.anti_alias = true;
    pixmap.fill_path(
        &path,
        &paint,
        FillRule::Winding,
        Transform::identity(),
        None,
    );
}

enum Dir { Up, Down, Left, Right }

/// Filled isosceles triangle pointing outward at (cx, cy).
fn compass_arrow(pixmap: &mut Pixmap, cx: f32, cy: f32, size: f32, dir: Dir, color: Color) {
    let (tip, b1, b2) = match dir {
        Dir::Up => (
            (cx, cy - size),
            (cx - size * 0.7, cy + size * 0.4),
            (cx + size * 0.7, cy + size * 0.4),
        ),
        Dir::Down => (
            (cx, cy + size),
            (cx - size * 0.7, cy - size * 0.4),
            (cx + size * 0.7, cy - size * 0.4),
        ),
        Dir::Left => (
            (cx - size, cy),
            (cx + size * 0.4, cy - size * 0.7),
            (cx + size * 0.4, cy + size * 0.7),
        ),
        Dir::Right => (
            (cx + size, cy),
            (cx - size * 0.4, cy - size * 0.7),
            (cx - size * 0.4, cy + size * 0.7),
        ),
    };
    let mut p = PathBuilder::new();
    p.move_to(tip.0, tip.1);
    p.line_to(b1.0, b1.1);
    p.line_to(b2.0, b2.1);
    p.close();
    let path = p.finish().unwrap();
    let mut paint = Paint::default();
    paint.set_color(color);
    paint.anti_alias = true;
    pixmap.fill_path(&path, &paint, FillRule::Winding, Transform::identity(), None);
}

#[allow(dead_code)]
fn chevron_v(pixmap: &mut Pixmap, cx: f32, cy: f32, size: f32, dir_y: f32, color: Color) {
    // dir_y = -1 points up, +1 points down
    let mut p = PathBuilder::new();
    p.move_to(cx - size, cy + dir_y * (-size * 0.5));
    p.line_to(cx, cy + dir_y * size * 0.5);
    p.line_to(cx + size, cy + dir_y * (-size * 0.5));
    let path = p.finish().unwrap();
    let mut paint = Paint::default();
    paint.set_color(color);
    paint.anti_alias = true;
    let stroke = Stroke {
        width: size * 0.3,
        line_cap: tiny_skia::LineCap::Round,
        line_join: tiny_skia::LineJoin::Round,
        ..Default::default()
    };
    pixmap.stroke_path(&path, &paint, &stroke, Transform::identity(), None);
}

fn chevron_h(pixmap: &mut Pixmap, cx: f32, cy: f32, size: f32, dir_x: f32, color: Color) {
    let mut p = PathBuilder::new();
    p.move_to(cx + dir_x * (-size * 0.5), cy - size);
    p.line_to(cx + dir_x * size * 0.5, cy);
    p.line_to(cx + dir_x * (-size * 0.5), cy + size);
    let path = p.finish().unwrap();
    let mut paint = Paint::default();
    paint.set_color(color);
    paint.anti_alias = true;
    let stroke = Stroke {
        width: size * 0.3,
        line_cap: tiny_skia::LineCap::Round,
        line_join: tiny_skia::LineJoin::Round,
        ..Default::default()
    };
    pixmap.stroke_path(&path, &paint, &stroke, Transform::identity(), None);
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
