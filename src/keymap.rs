//! The keymap cheat sheet: an SVG (such as keymap-drawer's) shown full screen
//! while `keymap_key` is held, one layer at a time.

use resvg::usvg::{Node, Rect, Tree};
use tiny_skia::{Color, FillRule, Paint, PathBuilder, PixmapMut, Transform};

use crate::config::{Config, Rgba};

/// Room around the layer, in screen pixels.
const MARGIN: f32 = 40.0;
/// Radius of the dots under the layer, one per layer.
const DOT: f32 = 3.5;

pub struct Keymap {
    tree: Tree,
    /// keymap-drawer puts each layer in its own top-level group: their index
    /// among the root's children, and their area. Empty: the whole image is
    /// one layer.
    layers: Vec<(usize, Rect)>,
    current: usize,
    bg: u32,
    dot: u32,
    dot_current: u32,
}

impl Keymap {
    pub fn load(path: &str, cfg: &Config) -> Result<Self, String> {
        let path = match path.strip_prefix("~/") {
            Some(rest) => format!("{}/{rest}", std::env::var("HOME").unwrap_or_default()),
            None => path.to_string(),
        };
        let data = std::fs::read(&path).map_err(|e| format!("{path}: {e}"))?;
        let mut opt = resvg::usvg::Options::default();
        opt.fontdb_mut().load_system_fonts();
        let tree = Tree::from_data(&data, &opt).map_err(|e| format!("{path}: {e}"))?;
        let layers = tree
            .root()
            .children()
            .iter()
            .enumerate()
            .filter_map(|(i, n)| match n {
                Node::Group(g) => Some((i, g.abs_layer_bounding_box().to_rect())),
                _ => None,
            })
            .collect();
        Ok(Self {
            tree,
            layers,
            current: 0,
            bg: cfg.theme.bg,
            dot: cfg.theme.text_dim & 0xffffff00 | 0x60,
            dot_current: cfg.theme.highlight,
        })
    }

    /// Back to the first layer, for the next time it shows.
    pub fn reset(&mut self) {
        self.current = 0;
    }

    pub fn next_layer(&mut self) {
        self.current = (self.current + 1) % self.layers.len().max(1);
    }

    /// The current layer as big as fits the screen, over the theme's
    /// background color. Every layer gets the same scale, so cycling doesn't
    /// jump.
    pub fn render(&self, pix: &mut PixmapMut) {
        let Rgba(r, g, b, a) = Rgba::from(self.bg);
        pix.fill(Color::from_rgba(r, g, b, a).unwrap_or(Color::BLACK));
        let (sw, sh) = (pix.width() as f32, pix.height() as f32);

        let (lw, lh) = if self.layers.is_empty() {
            (self.tree.size().width(), self.tree.size().height())
        } else {
            self.layers.iter().fold((0.0, 0.0), |(w, h), (_, r)| (r.width().max(w), r.height().max(h)))
        };
        // The dots sit in the bottom margin.
        let scale = ((sw - 2.0 * MARGIN) / lw).min((sh - 2.0 * MARGIN) / lh);
        if scale <= 0.0 {
            return;
        }
        let (iw, ih) = (lw * scale, lh * scale);
        let (x, y) = ((sw - iw) / 2.0, (sh - ih) / 2.0);

        match self.layers.get(self.current) {
            Some(&(i, r)) => {
                // Centered when it's smaller than the biggest layer.
                let (ox, oy) = (x + (iw - r.width() * scale) / 2.0, y + (ih - r.height() * scale) / 2.0);
                resvg::render_node(&self.tree.root().children()[i], Transform::from_scale(scale, scale).post_translate(ox, oy), pix);
            }
            None => {
                resvg::render(&self.tree, Transform::from_scale(scale, scale).post_translate(x, y), pix);
            }
        }

        if self.layers.len() > 1 {
            let n = self.layers.len() as f32;
            let step = DOT * 4.0;
            let (x0, cy) = (sw / 2.0 - (n - 1.0) * step / 2.0, sh - MARGIN / 2.0);
            for i in 0..self.layers.len() {
                let mut path = PathBuilder::new();
                path.push_circle(x0 + i as f32 * step, cy, DOT);
                fill(pix, path, if i == self.current { self.dot_current } else { self.dot });
            }
        }
    }
}

fn fill(pix: &mut PixmapMut, path: PathBuilder, color: u32) {
    let Some(path) = path.finish() else { return };
    let Rgba(r, g, b, a) = Rgba::from(color);
    let mut paint = Paint::default();
    paint.set_color_rgba8((r * 255.0) as u8, (g * 255.0) as u8, (b * 255.0) as u8, (a * 255.0) as u8);
    paint.anti_alias = true;
    pix.fill_path(&path, &paint, FillRule::Winding, Transform::identity(), None);
}
