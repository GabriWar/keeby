use std::path::PathBuf;

#[derive(Clone, Copy)]
pub struct Rgba(pub f32, pub f32, pub f32, pub f32);

impl Rgba {
    pub fn scale_alpha(self, k: f32) -> Self {
        Rgba(self.0, self.1, self.2, self.3 * k)
    }
}

#[derive(Clone, Copy, Debug)]
pub enum Anchor {
    TopLeft,
    Top,
    TopRight,
    Left,
    Center,
    Right,
    BottomLeft,
    Bottom,
    BottomRight,
}

#[derive(Clone, Copy, Debug)]
pub enum Orientation {
    Horizontal,
    Vertical,
}

pub struct Config {
    pub font_path: Option<String>,
    pub font_size: f32,
    pub margin_bottom: i32,
    pub pill_life_secs: f32,
    pub pill_fade_secs: f32,
    pub bg_color: Rgba,
    pub accent_color: Rgba,
    pub text_color: Rgba,
    pub text_dim_color: Rgba,

    pub mouse_enabled: bool,
    pub mouse_size: u32,
    pub mouse_anchor: Anchor,
    pub mouse_margin_x: i32,
    pub mouse_margin_y: i32,
    pub mouse_orientation: Orientation,
    pub mouse_show_arrow: bool,
    pub mouse_show_buttons: bool,
    pub mouse_show_scroll: bool,
    pub mouse_arrow_color: Rgba,

    pub monitor: Option<String>,
    pub auto_hide_keys: bool,
    pub auto_hide_keys_secs: f32,
    pub auto_hide_mouse: bool,
    pub auto_hide_mouse_secs: f32,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            font_path: None,
            font_size: 26.0,
            margin_bottom: 80,
            pill_life_secs: 3.5,
            pill_fade_secs: 1.0,
            bg_color: Rgba(0.13, 0.13, 0.17, 0.85),
            accent_color: Rgba(0.36, 0.32, 0.62, 0.92),
            text_color: Rgba(1.0, 1.0, 1.0, 1.0),
            text_dim_color: Rgba(0.82, 0.84, 0.90, 1.0),
            mouse_enabled: true,
            mouse_size: 110,
            mouse_anchor: Anchor::BottomLeft,
            mouse_margin_x: 30,
            mouse_margin_y: 80,
            mouse_orientation: Orientation::Horizontal,
            mouse_show_arrow: true,
            mouse_show_buttons: true,
            mouse_show_scroll: true,
            mouse_arrow_color: Rgba(1.0, 0.78, 0.32, 0.95),
            monitor: None,
            auto_hide_keys: true,
            auto_hide_keys_secs: 4.0,
            auto_hide_mouse: false,
            auto_hide_mouse_secs: 6.0,
        }
    }
}

impl Config {
    pub fn load() -> Self {
        let mut cfg = Config::default();
        let path = config_path();
        let Ok(contents) = std::fs::read_to_string(&path) else {
            // Write a starter config so it's discoverable.
            let _ = write_default(&path);
            return cfg;
        };
        for line in contents.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let Some((k, v)) = line.split_once('=') else {
                continue;
            };
            let k = k.trim();
            let v = v.trim().trim_matches('"').trim_matches('\'');
            match k {
                "font" => cfg.font_path = Some(v.to_string()),
                "size" | "font_size" => {
                    if let Ok(n) = v.parse::<f32>() {
                        cfg.font_size = n;
                    }
                }
                "margin_bottom" => {
                    if let Ok(n) = v.parse::<i32>() {
                        cfg.margin_bottom = n;
                    }
                }
                "life" | "pill_life" => {
                    if let Ok(n) = v.parse::<f32>() {
                        cfg.pill_life_secs = n;
                    }
                }
                "fade" | "pill_fade" => {
                    if let Ok(n) = v.parse::<f32>() {
                        cfg.pill_fade_secs = n;
                    }
                }
                "bg_color" | "color_bg" => {
                    if let Some(c) = parse_color(v) {
                        cfg.bg_color = c;
                    }
                }
                "accent_color" | "color_accent" => {
                    if let Some(c) = parse_color(v) {
                        cfg.accent_color = c;
                    }
                }
                "text_color" | "color_text" => {
                    if let Some(c) = parse_color(v) {
                        cfg.text_color = c;
                    }
                }
                "text_dim_color" | "color_text_dim" => {
                    if let Some(c) = parse_color(v) {
                        cfg.text_dim_color = c;
                    }
                }
                "mouse_enabled" => cfg.mouse_enabled = parse_bool(v),
                "mouse_size" => {
                    if let Ok(n) = v.parse::<u32>() {
                        cfg.mouse_size = n;
                    }
                }
                "mouse_anchor" => {
                    if let Some(a) = parse_anchor(v) {
                        cfg.mouse_anchor = a;
                    }
                }
                "mouse_margin_x" => {
                    if let Ok(n) = v.parse::<i32>() {
                        cfg.mouse_margin_x = n;
                    }
                }
                "mouse_margin_y" => {
                    if let Ok(n) = v.parse::<i32>() {
                        cfg.mouse_margin_y = n;
                    }
                }
                "mouse_orientation" => {
                    if let Some(o) = parse_orientation(v) {
                        cfg.mouse_orientation = o;
                    }
                }
                "mouse_show_arrow" => cfg.mouse_show_arrow = parse_bool(v),
                "mouse_show_buttons" => cfg.mouse_show_buttons = parse_bool(v),
                "mouse_show_scroll" => cfg.mouse_show_scroll = parse_bool(v),
                "mouse_arrow_color" => {
                    if let Some(c) = parse_color(v) {
                        cfg.mouse_arrow_color = c;
                    }
                }
                "monitor" => {
                    let v = v.trim();
                    cfg.monitor = if v.is_empty() || v.eq_ignore_ascii_case("auto") {
                        None
                    } else {
                        Some(v.to_string())
                    };
                }
                "auto_hide_keys" => cfg.auto_hide_keys = parse_bool(v),
                "auto_hide_keys_secs" => {
                    if let Ok(n) = v.parse::<f32>() {
                        cfg.auto_hide_keys_secs = n;
                    }
                }
                "auto_hide_mouse" => cfg.auto_hide_mouse = parse_bool(v),
                "auto_hide_mouse_secs" => {
                    if let Ok(n) = v.parse::<f32>() {
                        cfg.auto_hide_mouse_secs = n;
                    }
                }
                _ => {}
            }
        }
        cfg
    }

    pub fn resolve_font_bytes(&self) -> Vec<u8> {
        // 1. User-specified path.
        if let Some(p) = &self.font_path {
            if let Ok(b) = std::fs::read(p) {
                return b;
            }
            eprintln!("keeby: font {p} not readable, falling back");
        }
        // 2. Fallback chain (cozette first per default vibe).
        const PATHS: &[&str] = &[
            "/usr/share/fonts/TTF/CozetteVector.ttf",
            "/usr/share/fonts/TTF/JetBrainsMonoNerdFont-Bold.ttf",
            "/usr/share/fonts/TTF/JetBrainsMono-Bold.ttf",
            "/usr/share/fonts/TTF/JetBrainsMonoNerdFont-Regular.ttf",
            "/usr/share/fonts/TTF/JetBrainsMono-Regular.ttf",
            "/usr/share/fonts/noto/NotoSansMono-Regular.ttf",
            "/usr/share/fonts/noto/NotoSans-Regular.ttf",
        ];
        for p in PATHS {
            if let Ok(b) = std::fs::read(p) {
                return b;
            }
        }
        panic!("no font found — set `font = /path/to/font.ttf` in {}", config_path().display());
    }
}

fn config_path() -> PathBuf {
    let base = std::env::var("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
            PathBuf::from(home).join(".config")
        });
    base.join("keeby").join("config.conf")
}

fn write_default(path: &std::path::Path) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let body = "# keeby config
# font = path to .ttf/.otf (defaults to cozette → jetbrains → noto)
# size = font size in px (cozette is a pixel font, try multiples of 13)
# margin_bottom = pixels above bottom edge
# life = seconds a key pill stays before fading
# fade = seconds the fade animation lasts
#
# colors accept #rrggbb or #rrggbbaa. paste any hellwal/pywal hex here.
# bg_color      = older pill background
# accent_color  = newest pill background
# text_color    = newest pill text
# text_dim_color= older pill text

font = /usr/share/fonts/TTF/CozetteVector.ttf
size = 26
margin_bottom = 80
life = 3.5
fade = 1.0

bg_color       = #21222cd9
accent_color   = #5c52a0eb
text_color     = #ffffffff
text_dim_color = #d2d6e6ff

# --- mouse indicator ---
# mouse_enabled        = true | false
# mouse_size           = widget side length in px (square surface)
# mouse_anchor         = top-left | top | top-right | left | center | right | bottom-left | bottom | bottom-right
# mouse_margin_x/y     = pixels offset from anchor edges
# mouse_orientation    = horizontal | vertical (button row layout)
# mouse_show_arrow     = motion-direction arrow inside the cursor circle
# mouse_show_buttons   = LMB / MMB / RMB lights
# mouse_show_scroll    = scroll-wheel chevrons
# mouse_arrow_color    = motion arrow color

mouse_enabled       = true
mouse_size          = 110
mouse_anchor        = bottom-left
mouse_margin_x      = 30
mouse_margin_y      = 80
mouse_orientation   = horizontal
mouse_show_arrow    = true
mouse_show_buttons  = true
mouse_show_scroll   = true
mouse_arrow_color   = #ffc750f2

# --- monitor ---
# monitor = auto | <output name like DP-1, HDMI-A-1>
monitor = auto

# --- auto-hide ---
# attaches a null buffer after N seconds of no activity (no CPU/GPU cost while hidden)
auto_hide_keys      = true
auto_hide_keys_secs = 4.0
auto_hide_mouse     = false
auto_hide_mouse_secs = 6.0
";
    std::fs::write(path, body)
}

fn parse_bool(s: &str) -> bool {
    matches!(
        s.to_ascii_lowercase().as_str(),
        "1" | "true" | "yes" | "on" | "enabled"
    )
}

fn parse_anchor(s: &str) -> Option<Anchor> {
    let s = s.to_ascii_lowercase().replace('_', "-");
    Some(match s.as_str() {
        "top-left" | "topleft" | "tl" => Anchor::TopLeft,
        "top" | "t" => Anchor::Top,
        "top-right" | "topright" | "tr" => Anchor::TopRight,
        "left" | "l" => Anchor::Left,
        "center" | "centre" | "c" => Anchor::Center,
        "right" | "r" => Anchor::Right,
        "bottom-left" | "bottomleft" | "bl" => Anchor::BottomLeft,
        "bottom" | "b" => Anchor::Bottom,
        "bottom-right" | "bottomright" | "br" => Anchor::BottomRight,
        _ => return None,
    })
}

fn parse_orientation(s: &str) -> Option<Orientation> {
    match s.to_ascii_lowercase().as_str() {
        "horizontal" | "horiz" | "h" => Some(Orientation::Horizontal),
        "vertical" | "vert" | "v" => Some(Orientation::Vertical),
        _ => None,
    }
}

fn parse_color(s: &str) -> Option<Rgba> {
    let s = s.trim().trim_start_matches('#');
    let (r, g, b, a) = match s.len() {
        6 => {
            let r = u8::from_str_radix(&s[0..2], 16).ok()?;
            let g = u8::from_str_radix(&s[2..4], 16).ok()?;
            let b = u8::from_str_radix(&s[4..6], 16).ok()?;
            (r, g, b, 255u8)
        }
        8 => {
            let r = u8::from_str_radix(&s[0..2], 16).ok()?;
            let g = u8::from_str_radix(&s[2..4], 16).ok()?;
            let b = u8::from_str_radix(&s[4..6], 16).ok()?;
            let a = u8::from_str_radix(&s[6..8], 16).ok()?;
            (r, g, b, a)
        }
        _ => return None,
    };
    Some(Rgba(
        r as f32 / 255.0,
        g as f32 / 255.0,
        b as f32 / 255.0,
        a as f32 / 255.0,
    ))
}
