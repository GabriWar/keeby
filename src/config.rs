use std::path::PathBuf;

#[derive(Clone, Copy)]
pub struct Rgba(pub f32, pub f32, pub f32, pub f32);

impl Rgba {
    pub fn scale_alpha(self, k: f32) -> Self {
        Rgba(self.0, self.1, self.2, self.3 * k)
    }
}

impl From<u32> for Rgba {
    /// From `0xRRGGBBAA`.
    fn from(c: u32) -> Self {
        Rgba(
            (c >> 24 & 0xff) as f32 / 255.0,
            (c >> 16 & 0xff) as f32 / 255.0,
            (c >> 8 & 0xff) as f32 / 255.0,
            (c & 0xff) as f32 / 255.0,
        )
    }
}

/// The look shared by everything keeby draws: key pills, the mouse widget and
/// the pointer overlay. Colors are `0xRRGGBBAA`.
#[derive(Clone)]
pub struct Theme {
    /// Font file path or fontconfig family name.
    pub font: Option<String>,
    /// Older pill background, panels.
    pub bg: u32,
    /// Newest pill background, selectable areas.
    pub accent: u32,
    pub text: u32,
    pub text_dim: u32,
    /// Typed label prefix, pointer crosshair, motion arrow, drag marker.
    pub highlight: u32,
    /// Dims the screen behind the pointer overlay.
    pub shade: u32,
}

impl Default for Theme {
    fn default() -> Self {
        Self {
            font: None,
            bg: 0x21222cd9,
            accent: 0x5c52a0eb,
            text: 0xffffffff,
            text_dim: 0xd2d6e6ff,
            highlight: 0xffc750f2,
            shade: 0x22222266,
        }
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
    pub theme: Theme,
    pub font_size: f32,
    pub margin_bottom: i32,
    pub pill_life_secs: f32,
    pub pill_fade_secs: f32,

    pub mouse_enabled: bool,
    pub mouse_size: u32,
    pub mouse_anchor: Anchor,
    pub mouse_margin_x: i32,
    pub mouse_margin_y: i32,
    pub mouse_orientation: Orientation,
    pub mouse_show_arrow: bool,
    pub mouse_show_buttons: bool,
    pub mouse_show_scroll: bool,
    /// Defaults to the theme's highlight.
    pub mouse_arrow_color: Option<u32>,

    pub monitor: Option<String>,
    pub auto_hide_mouse: bool,
    pub auto_hide_mouse_secs: f32,

    /// Name of the key that toggles privacy mode (drops all input until pressed again).
    pub privacy_key: Option<String>,

    /// Optional whitelist of keyboard device-name substrings (case-insensitive).
    /// Empty = accept everything classified as a keyboard.
    pub keyboard_names: Vec<String>,
    /// Same for mouse devices.
    pub mouse_names: Vec<String>,

    /// `section.key = value` pointer settings, applied in order on top of the
    /// theme-derived defaults.
    pub pointer_settings: Vec<(String, String)>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            theme: Theme::default(),
            font_size: 26.0,
            margin_bottom: 80,
            pill_life_secs: 3.5,
            pill_fade_secs: 1.0,
            mouse_enabled: true,
            mouse_size: 110,
            mouse_anchor: Anchor::BottomLeft,
            mouse_margin_x: 30,
            mouse_margin_y: 80,
            mouse_orientation: Orientation::Horizontal,
            mouse_show_arrow: true,
            mouse_show_buttons: true,
            mouse_show_scroll: true,
            mouse_arrow_color: None,
            monitor: None,
            auto_hide_mouse: false,
            auto_hide_mouse_secs: 6.0,
            privacy_key: Some("Pause".to_string()),
            keyboard_names: Vec::new(),
            mouse_names: Vec::new(),
            pointer_settings: Vec::new(),
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
                "font" => cfg.theme.font = Some(v.to_string()),
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
                    if let Some(c) = parse_color_u32(v) {
                        cfg.theme.bg = c;
                    }
                }
                "accent_color" | "color_accent" => {
                    if let Some(c) = parse_color_u32(v) {
                        cfg.theme.accent = c;
                    }
                }
                "text_color" | "color_text" => {
                    if let Some(c) = parse_color_u32(v) {
                        cfg.theme.text = c;
                    }
                }
                "text_dim_color" | "color_text_dim" => {
                    if let Some(c) = parse_color_u32(v) {
                        cfg.theme.text_dim = c;
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
                "mouse_arrow_color" => cfg.mouse_arrow_color = parse_color_u32(v),
                "highlight_color" => {
                    if let Some(c) = parse_color_u32(v) {
                        cfg.theme.highlight = c;
                    }
                }
                "shade_color" => {
                    if let Some(c) = parse_color_u32(v) {
                        cfg.theme.shade = c;
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
                // Pills fade out on their own; kept so old configs still load.
                "auto_hide_keys" | "auto_hide_keys_secs" => {}
                "auto_hide_mouse" => cfg.auto_hide_mouse = parse_bool(v),
                "auto_hide_mouse_secs" => {
                    if let Ok(n) = v.parse::<f32>() {
                        cfg.auto_hide_mouse_secs = n;
                    }
                }
                "privacy_key" => {
                    let v = v.trim();
                    cfg.privacy_key = if v.is_empty()
                        || v.eq_ignore_ascii_case("none")
                        || v.eq_ignore_ascii_case("off")
                    {
                        None
                    } else {
                        Some(v.to_string())
                    };
                }
                "keyboards" | "keyboard_names" => {
                    cfg.keyboard_names = parse_name_list(v);
                }
                "mice" | "mouse_names" => {
                    cfg.mouse_names = parse_name_list(v);
                }
                _ if k.contains('.') => cfg.pointer_settings.push((k.to_string(), v.to_string())),
                _ => {}
            }
        }
        cfg
    }

    pub fn mouse_arrow(&self) -> Rgba {
        Rgba::from(self.mouse_arrow_color.unwrap_or(self.theme.highlight))
    }

    pub fn resolve_font_bytes(&self) -> Vec<u8> {
        // 1. The theme font: a path or a fontconfig family.
        if let Some(spec) = &self.theme.font {
            if let Some(b) = load_font(spec) {
                return b;
            }
            eprintln!("keeby: font {spec} not found, falling back");
        }
        // 2. The default chain.
        default_font().unwrap_or_else(|| {
            panic!("no font found — set `font = /path/to/font.ttf` in {}", config_path().display())
        })
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
#
# --- theme ---
# one palette for everything keeby draws: key pills, the mouse widget and the
# pointer overlay (`keeby point`). colors accept #rgb, #rgba, #rrggbb or
# #rrggbbaa. paste any hellwal/pywal hex here.
# font            = font file path or fontconfig family name (unset: the built-in
#                   Atkinson Hyperlegible Mono, made for telling look-alike letters apart)
# bg_color        = older pill background
# accent_color    = newest pill background, selectable pointer areas
# text_color      = newest pill text, pointer labels
# text_dim_color  = older pill text
# highlight_color = mouse arrow, typed label prefix, pointer crosshair
# shade_color     = dims the screen behind the pointer overlay

# font = /usr/share/fonts/TTF/CozetteVector.ttf
size = 26
margin_bottom = 80
life = 3.5
fade = 1.0

bg_color        = #21222cd9
accent_color    = #5c52a0eb
text_color      = #ffffffff
text_dim_color  = #d2d6e6ff
highlight_color = #ffc750f2
shade_color     = #22222266

# --- mouse indicator ---
# mouse_enabled        = true | false
# mouse_size           = widget side length in px (square surface)
# mouse_anchor         = top-left | top | top-right | left | center | right | bottom-left | bottom | bottom-right
# mouse_margin_x/y     = pixels offset from anchor edges
# mouse_orientation    = horizontal | vertical (button row layout)
# mouse_show_arrow     = motion-direction arrow inside the cursor circle
# mouse_show_buttons   = LMB / MMB / RMB lights
# mouse_show_scroll    = scroll-wheel chevrons
# mouse_arrow_color    = motion arrow color (defaults to highlight_color)

mouse_enabled       = true
mouse_size          = 110
mouse_anchor        = bottom-left
mouse_margin_x      = 30
mouse_margin_y      = 80
mouse_orientation   = horizontal
mouse_show_arrow    = true
mouse_show_buttons  = true
mouse_show_scroll   = true

# --- monitor ---
# monitor = auto | <output name like DP-1, HDMI-A-1>
monitor = auto

# --- auto-hide ---
# hide the mouse widget after N seconds without mouse activity (key pills
# already fade out on their own after `life` seconds)
auto_hide_mouse      = false
auto_hide_mouse_secs = 6.0

# --- privacy panic key ---
# pressing this key once stops keeby from registering ANY further keys/mouse;
# pressing again resumes. set to `none` to disable. case-insensitive.
# accepted: F1..F12, Pause, ScrollLock, Print, Insert, NumLock, Menu, CapsLock,
#           or any single letter (a..z) / digit (0..9).
privacy_key = Pause

# --- input device whitelist ---
# Comma-separated list of substrings (case-insensitive). If set, only devices
# whose name contains one of the substrings will be picked up. If empty,
# every keyboard, mouse, touchpad and tablet is used.
# Run `keeby --list` to see every device's name and what keeby makes of it.
# Examples:
#   keyboards = AT Translated, Corne
#   mice      = Logitech, Razer
keyboards =
mice      =

# --- pointer (keeby point) ---
# move, click and drag the pointer from the keyboard. options use the
# `section.key = value` form; colors left unset follow the theme above.
# run `keeby point --help-config` for every option.
general.modes = floating,click
mode_floating.source = detect
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

fn parse_name_list(s: &str) -> Vec<String> {
    s.split(',')
        .map(|p| p.trim().to_string())
        .filter(|p| !p.is_empty())
        .collect()
}

fn parse_orientation(s: &str) -> Option<Orientation> {
    match s.to_ascii_lowercase().as_str() {
        "horizontal" | "horiz" | "h" => Some(Orientation::Horizontal),
        "vertical" | "vert" | "v" => Some(Orientation::Vertical),
        _ => None,
    }
}

/// Parses `#rgb`, `#rgba`, `#rrggbb` or `#rrggbbaa` (the `#` is optional)
/// into `0xRRGGBBAA`.
pub fn parse_color_u32(s: &str) -> Option<u32> {
    let s = s.trim();
    let s = s.strip_prefix('#').unwrap_or(s);
    let d: Vec<u32> = s.chars().map(|c| c.to_digit(16)).collect::<Option<_>>()?;
    Some(match d.len() {
        8 => d.iter().fold(0, |acc, x| acc << 4 | x),
        6 => d.iter().fold(0, |acc, x| acc << 4 | x) << 8 | 0xff,
        4 => d.iter().fold(0, |acc, x| acc << 8 | x << 4 | x),
        3 => d.iter().fold(0, |acc, x| acc << 8 | x << 4 | x) << 8 | 0xff,
        _ => return None,
    })
}

/// The built-in font: Atkinson Hyperlegible Mono (Braille Institute), made so
/// look-alike characters such as l/1/I and O/0 stay distinct; monospaced so
/// labels line up. SIL Open Font License, see assets/fonts/OFL.txt.
pub const DEFAULT_FONT: &[u8] = include_bytes!("../assets/fonts/AtkinsonHyperlegibleMono-Regular.ttf");

pub fn default_font() -> Option<Vec<u8>> {
    Some(DEFAULT_FONT.to_vec())
}

/// Loads a font from a file path (`~` allowed), or else asks fontconfig for
/// the family. An empty spec means the default chain.
pub fn load_font(spec: &str) -> Option<Vec<u8>> {
    if spec.is_empty() {
        return default_font();
    }
    let path = match spec.strip_prefix("~/") {
        Some(rest) => format!("{}/{rest}", std::env::var("HOME").unwrap_or_default()),
        None => spec.to_string(),
    };
    if let Ok(b) = std::fs::read(&path) {
        return Some(b);
    }
    let out = std::process::Command::new("fc-match")
        .args(["-f", "%{file}", spec])
        .output()
        .ok()?;
    std::fs::read(String::from_utf8_lossy(&out.stdout).trim()).ok()
}
