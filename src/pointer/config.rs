// SPDX-License-Identifier: GPL-3.0-only
// Ported from wl-kbptr (https://github.com/moverest/wl-kbptr) by moverest.
//! Pointer settings. They live in keeby's config as `section.key = value`
//! lines with wl-kbptr's section and option names, and `-o section.key=value`
//! sets them per run. Colors left unset follow keeby's theme.

use crate::config::{parse_color_u32, Theme};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Click {
    None,
    Left,
    Right,
    Middle,
    Drag,
}

impl Click {
    /// Linux input button code (BTN_LEFT, BTN_RIGHT, BTN_MIDDLE).
    pub fn button_code(self) -> u32 {
        match self {
            Click::Right => 0x111,
            Click::Middle => 0x112,
            _ => 0x110,
        }
    }

    pub fn letter(self) -> char {
        match self {
            Click::None => 'n',
            Click::Left => 'l',
            Click::Right => 'r',
            Click::Middle => 'm',
            Click::Drag => 'd',
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FloatingSource {
    Stdin,
    Detect,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeySource {
    Wayland,
    Evdev,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MarkerShape {
    Circle,
    Caret,
    Rectangle,
}

/// `<min> <pct>% <max>`: a share of the area's height, clamped.
#[derive(Clone, Copy, Debug)]
pub struct RelativeFontSize {
    pub min: f64,
    pub proportion: f64,
    pub max: f64,
}

impl RelativeFontSize {
    pub fn compute(&self, height: f64) -> f64 {
        (height * self.proportion).max(self.min).min(self.max)
    }
}

#[derive(Clone)]
pub struct General {
    pub home_row_keys: Option<Vec<String>>,
    pub modes: String,
    pub cancellation_status_code: u8,
    pub key_source: KeySource,
}

#[derive(Clone)]
pub struct Labels {
    pub label_color: u32,
    pub label_select_color: u32,
    pub unselectable_bg_color: u32,
    pub selectable_bg_color: u32,
    pub selectable_border_color: u32,
    pub label_font_family: String,
    pub label_font_size: RelativeFontSize,
    pub label_symbols: String,
}

#[derive(Clone)]
pub struct Floating {
    pub source: FloatingSource,
    pub labels: Labels,
    /// Grow labels of small targets up to `label_min_size` where that
    /// doesn't run into other targets or labels.
    pub label_grow: bool,
    pub label_min_size: f64,
    /// Backdrop behind a grown label that spills out of its target.
    pub label_plate_color: u32,
}

#[derive(Clone)]
pub struct Bisect {
    pub label_color: u32,
    pub label_font_size: f64,
    pub label_font_family: String,
    pub label_padding: f64,
    pub pointer_size: f64,
    pub pointer_color: u32,
    pub unselectable_bg_color: u32,
    pub even_area_bg_color: u32,
    pub even_area_border_color: u32,
    pub odd_area_bg_color: u32,
    pub odd_area_border_color: u32,
    pub history_border_color: u32,
}

#[derive(Clone)]
pub struct Split {
    pub pointer_size: f64,
    pub pointer_color: u32,
    pub bg_color: u32,
    pub area_bg_color: u32,
    pub vertical_color: u32,
    pub horizontal_color: u32,
    pub history_border_color: u32,
}

#[derive(Clone)]
pub struct Drag {
    pub start_marker_color: u32,
    pub start_marker_size: f64,
    pub start_marker_shape: MarkerShape,
}

#[derive(Clone)]
pub struct PointerConfig {
    pub general: General,
    pub tile: Labels,
    pub floating: Floating,
    pub bisect: Bisect,
    pub split: Split,
    pub click_button: Click,
    pub drag: Drag,
}

fn with_alpha(c: u32, a: u8) -> u32 {
    (c & 0xffff_ff00) | a as u32
}

impl PointerConfig {
    /// Defaults, with colors and font taken from the theme.
    pub fn new(theme: &Theme) -> Self {
        // Empty means keeby's default font chain.
        let font = theme.font.clone().unwrap_or_default();
        let labels = |min: f64| Labels {
            label_color: theme.text,
            label_select_color: theme.highlight,
            unselectable_bg_color: theme.shade,
            selectable_bg_color: with_alpha(theme.target, 0x40),
            selectable_border_color: with_alpha(theme.target, 0xcc),
            label_font_family: font.clone(),
            label_font_size: RelativeFontSize { min, proportion: 0.5, max: 100.0 },
            label_symbols: "abcdefghijklmnopqrstuvwxyz".to_string(),
        };
        Self {
            general: General {
                home_row_keys: None,
                modes: "tile,bisect".to_string(),
                cancellation_status_code: 0,
                key_source: KeySource::Evdev,
            },
            tile: labels(8.0),
            floating: Floating {
                source: FloatingSource::Stdin,
                labels: labels(12.0),
                label_grow: true,
                label_min_size: 14.0,
                label_plate_color: with_alpha(theme.bg, 0xdd),
            },
            bisect: Bisect {
                label_color: theme.text,
                label_font_size: 20.0,
                label_font_family: font.clone(),
                label_padding: 12.0,
                pointer_size: 20.0,
                pointer_color: theme.highlight,
                unselectable_bg_color: theme.shade,
                even_area_bg_color: with_alpha(theme.target, 0x40),
                even_area_border_color: with_alpha(theme.target, 0x80),
                odd_area_bg_color: with_alpha(theme.text_dim, 0x30),
                odd_area_border_color: with_alpha(theme.text_dim, 0x80),
                history_border_color: with_alpha(theme.text_dim, 0x99),
            },
            split: Split {
                pointer_size: 20.0,
                pointer_color: theme.highlight,
                bg_color: theme.shade,
                area_bg_color: with_alpha(theme.bg, 0x88),
                vertical_color: with_alpha(theme.target, 0xcc),
                horizontal_color: with_alpha(theme.highlight, 0xcc),
                history_border_color: with_alpha(theme.text_dim, 0x99),
            },
            click_button: Click::Left,
            drag: Drag {
                start_marker_color: theme.highlight,
                start_marker_size: 8.0,
                start_marker_shape: MarkerShape::Caret,
            },
        }
    }

    /// Sets `section.key` (a bare key means `general.key`), like wl-kbptr's
    /// `-o` option.
    pub fn set(&mut self, key: &str, value: &str) -> Result<(), String> {
        let (section, field) = key.split_once('.').unwrap_or(("general", key));
        let bad = || format!("Invalid value `{value}` for {section}.{field}.");
        let color = || parse_color_u32(value).ok_or_else(bad);
        let num = || value.trim().parse::<f64>().map_err(|_| bad());
        let s = || value.to_string();

        match section {
            "general" => match field {
                "home_row_keys" => self.general.home_row_keys = parse_home_row_keys(value)?,
                "modes" => self.general.modes = s(),
                "cancellation_status_code" => {
                    self.general.cancellation_status_code = value
                        .trim()
                        .parse::<u8>()
                        .map_err(|_| "Value should be between 0 and 255 (included).".to_string())?
                }
                "key_source" => {
                    self.general.key_source = match value {
                        "wayland" => KeySource::Wayland,
                        "evdev" => KeySource::Evdev,
                        _ => {
                            return Err(format!(
                                "Invalid key source '{value}'. Should be 'wayland' or 'evdev'."
                            ))
                        }
                    }
                }
                _ => return Err(unknown(section, field)),
            },
            "mode_tile" | "mode_floating" => {
                if section == "mode_floating" {
                    match field {
                        "label_grow" => {
                            self.floating.label_grow = match value.trim() {
                                "true" | "yes" | "on" | "1" => true,
                                "false" | "no" | "off" | "0" => false,
                                _ => return Err(bad()),
                            };
                            return Ok(());
                        }
                        "label_min_size" => {
                            self.floating.label_min_size = num()?;
                            return Ok(());
                        }
                        "label_plate_color" => {
                            self.floating.label_plate_color = color()?;
                            return Ok(());
                        }
                        _ => {}
                    }
                }
                if section == "mode_floating" && field == "source" {
                    self.floating.source = match value {
                        "stdin" => FloatingSource::Stdin,
                        "detect" => FloatingSource::Detect,
                        _ => {
                            return Err(format!(
                                "Invalid source '{value}'. Should be 'stdin' or 'detect'."
                            ))
                        }
                    };
                    return Ok(());
                }
                let l = if section == "mode_tile" { &mut self.tile } else { &mut self.floating.labels };
                match field {
                    "label_color" => l.label_color = color()?,
                    "label_select_color" => l.label_select_color = color()?,
                    "unselectable_bg_color" => l.unselectable_bg_color = color()?,
                    "selectable_bg_color" => l.selectable_bg_color = color()?,
                    "selectable_border_color" => l.selectable_border_color = color()?,
                    "label_font_family" => l.label_font_family = s(),
                    "label_font_size" => l.label_font_size = parse_relative_font_size(value)?,
                    "label_symbols" => l.label_symbols = s(),
                    _ => return Err(unknown(section, field)),
                }
            }
            "mode_bisect" => {
                let b = &mut self.bisect;
                match field {
                    "label_color" => b.label_color = color()?,
                    "label_font_size" => b.label_font_size = num()?,
                    "label_font_family" => b.label_font_family = s(),
                    "label_padding" => b.label_padding = num()?,
                    "pointer_size" => b.pointer_size = num()?,
                    "pointer_color" => b.pointer_color = color()?,
                    "unselectable_bg_color" => b.unselectable_bg_color = color()?,
                    "even_area_bg_color" => b.even_area_bg_color = color()?,
                    "even_area_border_color" => b.even_area_border_color = color()?,
                    "odd_area_bg_color" => b.odd_area_bg_color = color()?,
                    "odd_area_border_color" => b.odd_area_border_color = color()?,
                    "history_border_color" => b.history_border_color = color()?,
                    _ => return Err(unknown(section, field)),
                }
            }
            "mode_split" => {
                let p = &mut self.split;
                match field {
                    "pointer_size" => p.pointer_size = num()?,
                    "pointer_color" => p.pointer_color = color()?,
                    "bg_color" => p.bg_color = color()?,
                    "area_bg_color" => p.area_bg_color = color()?,
                    "vertical_color" => p.vertical_color = color()?,
                    "horizontal_color" => p.horizontal_color = color()?,
                    "history_border_color" => p.history_border_color = color()?,
                    _ => return Err(unknown(section, field)),
                }
            }
            "mode_click" => match field {
                "button" => {
                    self.click_button = match value {
                        "none" => Click::None,
                        "left" => Click::Left,
                        "right" => Click::Right,
                        "middle" => Click::Middle,
                        _ => {
                            return Err(format!(
                                "Invalid click button '{value}'. Should be 'left', 'right', 'middle' or 'none'."
                            ))
                        }
                    }
                }
                _ => return Err(unknown(section, field)),
            },
            "mode_drag" => {
                let d = &mut self.drag;
                match field {
                    "start_marker_color" => d.start_marker_color = color()?,
                    "start_marker_size" => d.start_marker_size = num()?,
                    "start_marker_shape" => {
                        d.start_marker_shape = match value {
                            "circle" => MarkerShape::Circle,
                            "caret" => MarkerShape::Caret,
                            "rectangle" => MarkerShape::Rectangle,
                            _ => {
                                return Err(format!(
                                    "Invalid drag marker shape '{value}'. Should be 'circle', 'caret' or 'rectangle'."
                                ))
                            }
                        }
                    }
                    _ => return Err(unknown(section, field)),
                }
            }
            _ => return Err(format!("Configuration section `{section}` does not exist.")),
        }
        Ok(())
    }

    /// Applies `-o` style `section.key=value`.
    pub fn set_option(&mut self, opt: &str) -> Result<(), String> {
        let (k, v) = opt
            .split_once('=')
            .ok_or_else(|| format!("Invalid configuration parameter `{opt}`."))?;
        self.set(k, v)
    }

    /// Loads a wl-kbptr style INI file (`[section]` then `key=value`), for
    /// `-c FILE`.
    pub fn load_ini(&mut self, path: &str) -> Result<(), String> {
        let text =
            std::fs::read_to_string(path).map_err(|_| format!("Could not open config file '{path}'"))?;
        let mut section = "general".to_string();
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            if let Some(name) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
                section = name.to_string();
                continue;
            }
            let (k, v) = line
                .split_once('=')
                .ok_or_else(|| "Unexpected end of line. Expected equal operator.".to_string())?;
            self.set(&format!("{section}.{}", k.trim_end()), v)?;
        }
        Ok(())
    }
}

fn unknown(section: &str, field: &str) -> String {
    format!("Configuration option `{section}.{field}` does not exist.")
}

fn parse_home_row_keys(value: &str) -> Result<Option<Vec<String>>, String> {
    if value.is_empty() {
        return Ok(None);
    }
    let keys: Vec<String> = value.chars().map(|c| c.to_string()).collect();
    if keys.len() < 11 {
        return Err("Could not parse home row keys. Not enough characters. Exactly 11 are required. \
                    The first 8 are used to select the sub-areas in the bisect mode. The next three \
                    characters are used for left, right and middle click respectively."
            .to_string());
    }
    if keys.len() > 11 {
        return Err("Too many characters. Exactly 11 are required.".to_string());
    }
    Ok(Some(keys))
}

fn parse_relative_font_size(value: &str) -> Result<RelativeFontSize, String> {
    let mut it = value.split_whitespace();
    let num = |t: Option<&str>, what: &str| -> Result<(f64, bool), String> {
        let t = t.ok_or_else(|| format!("No {what} size."))?;
        let pct = t.ends_with('%');
        let n = t.trim_end_matches('%').parse::<f64>().map_err(|_| format!("Invalid {what} size."))?;
        Ok((n, pct))
    };
    let (min, _) = num(it.next(), "minimum")?;
    let (prop, pct) = num(it.next(), "relative")?;
    let (max, _) = num(it.next(), "maximum")?;
    let proportion = if pct { prop / 100.0 } else { prop };
    if min < 0.0 {
        return Err("Minimum size can't be negative.".to_string());
    }
    if max < 0.0 {
        return Err("Maximum size can't be negative.".to_string());
    }
    if proportion < 0.0 {
        return Err("Proportion can't be negative.".to_string());
    }
    if min > max {
        return Err("Minimum size can't be greater than maximum size.".to_string());
    }
    Ok(RelativeFontSize { min, proportion, max })
}

/// The documented option list, printed by `keeby point --help-config`.
pub const HELP: &str = "\
# Pointer options. Put them in ~/.config/keeby/config.conf as `section.key = value`,
# or pass them per run with `-o section.key=value`. Colors you don't set follow the
# theme (bg_color, target_color, text_color, text_dim_color, highlight_color,
# shade_color, font).

## general
# general.modes                     modes to chain: tile, floating, bisect, split, click   (tile,bisect)
# general.home_row_keys             11 characters: 8 for bisect areas, then left/right/middle click
#                                   (empty = a s d f j k l m g h b on your keyboard layout)
# general.cancellation_status_code  exit status when cancelled   (0)
# general.key_source                evdev: grab the keyboard without taking the focus, so open
#                                   menus stay open (needs the `input` group, falls back to wayland)
#                                   wayland: take the keyboard focus   (evdev)

## tile and floating labels  (section mode_tile or mode_floating)
# label_color, label_select_color, unselectable_bg_color, selectable_bg_color,
# selectable_border_color   colors (#rgb, #rgba, #rrggbb or #rrggbbaa)
# label_font_family         font family or path to a font file   (theme font)
# label_font_size           `<min> <pct>% <max>` of the area's height   (tile 8 50% 100, floating 12 50% 100)
# label_symbols             characters used in labels   (abcdefghijklmnopqrstuvwxyz)
# mode_floating.source      stdin: read `wxh+x+y` areas from stdin, detect: find targets on screen   (stdin)
# mode_floating.label_grow  grow labels of small targets to label_min_size where there's room   (true)
# mode_floating.label_min_size     smallest label size worth growing to, in px   (14)
# mode_floating.label_plate_color  backdrop behind a grown label that spills out of its target

## mode_bisect
# label_color, pointer_color, unselectable_bg_color, even_area_bg_color, even_area_border_color,
# odd_area_bg_color, odd_area_border_color, history_border_color   colors
# label_font_size (20), label_font_family, label_padding (12), pointer_size (20)

## mode_split
# pointer_color, bg_color, area_bg_color, vertical_color, horizontal_color, history_border_color   colors
# pointer_size (20)

## mode_click
# mode_click.button         left, right, middle or none   (left)

## mode_drag
# start_marker_color, start_marker_size (8), start_marker_shape: caret, circle or rectangle   (caret)
";
