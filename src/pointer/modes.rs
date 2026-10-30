// SPDX-License-Identifier: GPL-3.0-only
// Ported from wl-kbptr (https://github.com/moverest/wl-kbptr) by moverest.
//! The selection modes and their chaining, ported from wl-kbptr's `mode*.c`.
//! Behavior (areas, labels, keys, drag and double click) matches it exactly;
//! only the drawing goes through tiny-skia instead of cairo.

use tiny_skia::{LineCap, LineJoin};
use xkbcommon::xkb::Keysym;

use super::config::{Click, FloatingSource, Labels, MarkerShape, PointerConfig};
use super::draw::{Canvas, Fonts, Op};
use super::label::{LabelSelection, LabelSymbols};

pub const MAX_NUM_MODES: usize = 3;
const BISECT_MAX_HISTORY: usize = 16;
const SPLIT_MAX_HISTORY: usize = 32;
const HOME_ROW_LEN: usize = 8;
const HOME_ROW_LEFT_CLICK: usize = 8;
const HOME_ROW_RIGHT_CLICK: usize = 9;
const HOME_ROW_MIDDLE_CLICK: usize = 10;
const MIN_SUB_AREA_SIZE: i32 = 25 * 50;
const DIVIDE_8_RATIO: f64 = 1.8;
const ARROW_SIZE: f64 = 5.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ModeKind {
    Tile,
    Floating,
    Bisect,
    Split,
    Click,
}

pub fn parse_modes(s: &str) -> Result<Vec<ModeKind>, String> {
    let mut out = Vec::new();
    for tok in s.split(',').filter(|t| !t.is_empty()) {
        if out.len() >= MAX_NUM_MODES {
            return Err(format!("Only {MAX_NUM_MODES} modes can be specified."));
        }
        out.push(match tok {
            "tile" => ModeKind::Tile,
            "floating" => ModeKind::Floating,
            "bisect" => ModeKind::Bisect,
            "split" => ModeKind::Split,
            "click" => ModeKind::Click,
            _ => return Err(format!("Unknown mode '{tok}'.")),
        });
    }
    Ok(out)
}

/// Where floating mode gets its areas from.
pub trait AreaSource {
    fn areas(&mut self, source: FloatingSource, area: Rect) -> Vec<Rect>;
}

struct TileState {
    area: Rect,
    sub_w: i32,
    sub_h: i32,
    cols: i32,
    rows: i32,
    w_off: i32,
    h_off: i32,
    symbols: LabelSymbols,
    selection: LabelSelection,
}

impl TileState {
    fn idx_to_rect(&self, idx: i32) -> Rect {
        let column = idx / self.rows;
        let row = idx % self.rows;
        Rect {
            x: column * self.sub_w + column.min(self.w_off) + self.area.x,
            w: self.sub_w + i32::from(column < self.w_off),
            y: row * self.sub_h + row.min(self.h_off) + self.area.y,
            h: self.sub_h + i32::from(row < self.h_off),
        }
    }
}

struct FloatingState {
    areas: Vec<Rect>,
    symbols: LabelSymbols,
    selection: LabelSelection,
}

struct HistState {
    areas: Vec<Rect>,
    current: usize,
}

enum ModeState {
    Tile(TileState),
    Floating(FloatingState),
    Bisect(HistState),
    Split(HistState),
    Click,
}

pub struct Engine {
    pub cfg: PointerConfig,
    kinds: Vec<ModeKind>,
    states: Vec<Option<ModeState>>,
    /// Index of the mode being run; `None` before the first one.
    current: Option<usize>,
    pub running: bool,
    pub click: Click,
    pub drag: bool,
    pub double_click: bool,
    drag_phase: u8,
    pub drag_start: (i32, i32),
    pub drag_button: Click,
    pub result: Option<Rect>,
    home_row: Vec<String>,
    /// Latest pointer position requested while selecting (bisect, split).
    pub pending_move: Option<(i32, i32)>,
}

impl Engine {
    pub fn new(cfg: PointerConfig, home_row: Vec<String>, drag: bool, double_click: bool) -> Result<Self, String> {
        let kinds = parse_modes(&cfg.general.modes)?;
        let home_row = cfg.general.home_row_keys.clone().unwrap_or(home_row);
        Ok(Self {
            cfg,
            states: kinds.iter().map(|_| None).collect(),
            kinds,
            current: None,
            running: true,
            click: Click::None,
            drag,
            double_click,
            drag_phase: 0,
            drag_start: (0, 0),
            drag_button: Click::Left,
            result: None,
            home_row,
            pending_move: None,
        })
    }

    pub fn has_last_mode_returned(&self) -> bool {
        matches!(self.current, Some(c) if c >= self.kinds.len())
    }

    fn chain_includes_floating(&self) -> bool {
        self.kinds.contains(&ModeKind::Floating)
    }

    fn move_to_center(&mut self, r: Rect) {
        self.pending_move = Some((r.x + r.w / 2, r.y + r.h / 2));
    }

    pub fn enter_next_mode(&mut self, area: Rect, src: &mut dyn AreaSource) {
        if self.has_last_mode_returned() {
            return;
        }
        let next = self.current.map_or(0, |c| c + 1);
        self.current = Some(next);

        if next >= self.kinds.len() {
            if self.drag && self.drag_phase == 0 {
                let floating = self.chain_includes_floating();
                self.drag_start = (if floating { area.x } else { area.x + area.w / 2 }, area.y + area.h / 2);
                self.drag_phase = 1;
                for i in 0..next {
                    self.restart(i);
                }
                self.current = Some(0);
            } else if self.drag && self.drag_phase == 1 {
                let floating = self.chain_includes_floating();
                self.drag_phase = 0;
                // Drag with the button picked by click mode or a click key.
                self.drag_button = if self.click == Click::None { Click::Left } else { self.click };
                self.click = Click::Drag;
                self.result = Some(Rect {
                    x: if floating { area.x + area.w - 1 } else { area.x + area.w / 2 },
                    y: area.y + area.h / 2,
                    w: 1,
                    h: 1,
                });
            } else {
                self.result = Some(area);
            }
            return;
        }

        let state = self.enter(self.kinds[next], area, src);
        self.states[next] = Some(state);
    }

    fn labels_state(&mut self, cfg: &Labels, count: usize) -> Option<(LabelSymbols, LabelSelection)> {
        match LabelSymbols::from_str(&cfg.label_symbols) {
            Ok(symbols) => {
                let selection = LabelSelection::new(&symbols, count);
                Some((symbols, selection))
            }
            Err(e) => {
                eprintln!("keeby: {e}");
                self.running = false;
                None
            }
        }
    }

    fn enter(&mut self, kind: ModeKind, area: Rect, src: &mut dyn AreaSource) -> ModeState {
        match kind {
            ModeKind::Tile => {
                let max_num_sub_areas = 26 * 26;
                let sub_area_size = (area.w * area.h / max_num_sub_areas).max(MIN_SUB_AREA_SIZE);
                let mut sub_h = (sub_area_size as f64 / 2.0).sqrt() as i32;
                let rows = (area.h / sub_h.max(1)).max(1);
                let h_off = area.h % rows;
                sub_h = area.h / rows;
                let mut sub_w = ((sub_area_size * 2) as f64).sqrt() as i32;
                let cols = (area.w / sub_w.max(1)).max(1);
                let w_off = area.w % cols;
                sub_w = area.w / cols;
                let cfg = self.cfg.tile.clone();
                let Some((symbols, selection)) = self.labels_state(&cfg, (rows * cols) as usize) else {
                    return ModeState::Click;
                };
                ModeState::Tile(TileState { area, sub_w, sub_h, cols, rows, w_off, h_off, symbols, selection })
            }
            ModeKind::Floating => {
                let cfg = self.cfg.floating.labels.clone();
                let areas = src.areas(self.cfg.floating.source, area);
                let Some((symbols, selection)) = self.labels_state(&cfg, areas.len()) else {
                    return ModeState::Click;
                };
                ModeState::Floating(FloatingState { areas, symbols, selection })
            }
            ModeKind::Bisect | ModeKind::Split => {
                self.move_to_center(area);
                let st = HistState { areas: vec![area], current: 0 };
                if kind == ModeKind::Bisect {
                    ModeState::Bisect(st)
                } else {
                    ModeState::Split(st)
                }
            }
            ModeKind::Click => {
                self.click = self.cfg.click_button;
                self.enter_next_mode(area, src);
                ModeState::Click
            }
        }
    }

    fn restart(&mut self, i: usize) {
        let mut moved = None;
        match self.states[i].as_mut() {
            Some(ModeState::Tile(s)) => s.selection.clear(),
            Some(ModeState::Floating(s)) => s.selection.clear(),
            Some(ModeState::Bisect(s)) | Some(ModeState::Split(s)) => {
                s.current = 0;
                moved = Some(s.areas[0]);
            }
            _ => {}
        }
        if let Some(r) = moved {
            self.move_to_center(r);
        }
    }

    fn reenter(&mut self, i: usize) {
        let mut moved = None;
        match self.states[i].as_mut() {
            Some(ModeState::Tile(s)) => {
                s.selection.back();
            }
            Some(ModeState::Floating(s)) => {
                s.selection.back();
            }
            Some(ModeState::Bisect(s)) | Some(ModeState::Split(s)) => moved = Some(s.areas[s.current]),
            Some(ModeState::Click) => {
                self.reenter_prev_mode();
            }
            None => {}
        }
        if let Some(r) = moved {
            self.move_to_center(r);
        }
    }

    pub fn reenter_prev_mode(&mut self) -> bool {
        let Some(c) = self.current else { return false };
        if c == 0 {
            return false;
        }
        self.states[c] = None;
        self.current = Some(c - 1);
        self.reenter(c - 1);
        true
    }

    /// Finds the home row key for `text`; an uppercase click key also matches
    /// and turns on double click.
    fn find_home_row_key(&mut self, text: &str) -> Option<usize> {
        if let Some(i) = self.home_row.iter().position(|k| k == text) {
            return Some(i);
        }
        let mut chars = text.chars();
        let (Some(c), None) = (chars.next(), chars.next()) else { return None };
        if !c.is_ascii_uppercase() {
            return None;
        }
        let lower = c.to_ascii_lowercase().to_string();
        let i = self.home_row.iter().position(|k| *k == lower)?;
        if i < HOME_ROW_LEN {
            return None;
        }
        self.double_click = true;
        Some(i)
    }

    /// Handles a key press. Returns true when the overlay needs a redraw.
    pub fn key(&mut self, sym: Keysym, text: &str, src: &mut dyn AreaSource) -> bool {
        if self.has_last_mode_returned() {
            return false;
        }
        let Some(cur) = self.current else { return false };
        match self.kinds[cur] {
            ModeKind::Tile | ModeKind::Floating => self.labels_key(cur, sym, text, src),
            ModeKind::Bisect => self.bisect_key(cur, sym, text, src),
            ModeKind::Split => self.split_key(cur, sym, text, src),
            ModeKind::Click => false,
        }
    }

    fn labels_key(&mut self, cur: usize, sym: Keysym, text: &str, src: &mut dyn AreaSource) -> bool {
        if sym == Keysym::BackSpace {
            return match self.states[cur].as_mut() {
                Some(ModeState::Tile(s)) => s.selection.back(),
                Some(ModeState::Floating(s)) => s.selection.back(),
                _ => false,
            };
        }
        if sym == Keysym::Escape {
            self.running = false;
            return false;
        }
        let next_area = match self.states[cur].as_mut() {
            Some(ModeState::Tile(s)) => {
                let Some(i) = s.symbols.find_idx(text) else { return false };
                s.selection.append(i);
                s.selection.to_idx().map(|idx| s.idx_to_rect(idx as i32))
            }
            Some(ModeState::Floating(s)) => {
                let Some(i) = s.symbols.find_idx(text) else { return false };
                s.selection.append(i);
                s.selection.to_idx().map(|idx| s.areas[idx])
            }
            _ => return false,
        };
        if let Some(area) = next_area {
            self.enter_next_mode(area, src);
        }
        true
    }

    fn hist(&mut self, cur: usize) -> &mut HistState {
        match self.states[cur].as_mut() {
            Some(ModeState::Bisect(s)) | Some(ModeState::Split(s)) => s,
            _ => unreachable!("bisect/split state"),
        }
    }

    fn click_key(&mut self, i: usize) -> Option<Click> {
        match i {
            HOME_ROW_LEFT_CLICK => Some(Click::Left),
            HOME_ROW_RIGHT_CLICK => Some(Click::Right),
            HOME_ROW_MIDDLE_CLICK => Some(Click::Middle),
            _ => None,
        }
    }

    fn bisect_key(&mut self, cur: usize, sym: Keysym, text: &str, src: &mut dyn AreaSource) -> bool {
        if sym == Keysym::Escape {
            self.running = false;
            return false;
        }
        if sym == Keysym::Return || sym == Keysym::space {
            let a = { let s = self.hist(cur); s.areas[s.current] };
            self.enter_next_mode(a, src);
            return true;
        }
        if sym == Keysym::BackSpace {
            let s = self.hist(cur);
            if s.current > 0 {
                s.current -= 1;
            } else {
                self.reenter_prev_mode();
            }
            return true;
        }
        if self.hist(cur).current + 1 >= BISECT_MAX_HISTORY {
            return false;
        }
        let area = { let s = self.hist(cur); s.areas[s.current] };
        let division = determine_division(&area);
        let Some(matched) = self.find_home_row_key(text) else { return false };
        if matched >= HOME_ROW_LEN {
            if let Some(c) = self.click_key(matched) {
                self.click = c;
            }
            self.enter_next_mode(area, src);
            return false;
        }
        if division == Division::Undividable {
            return false;
        }
        if let Some(new_area) = division.idx_to_sub_area(matched, &area) {
            let s = self.hist(cur);
            s.areas.truncate(s.current + 1);
            s.areas.push(new_area);
            s.current += 1;
            self.move_to_center(new_area);
            return true;
        }
        false
    }

    fn split(&mut self, cur: usize, dir: SplitDir) -> bool {
        let s = self.hist(cur);
        if s.current + 1 >= SPLIT_MAX_HISTORY {
            return false;
        }
        let area = s.areas[s.current];
        let mut new_area = area;
        match dir {
            SplitDir::Right | SplitDir::Left => {
                if area.w <= 1 {
                    return false;
                }
                new_area.w = area.w / 2;
                if dir == SplitDir::Right {
                    new_area.x += new_area.w;
                }
            }
            SplitDir::Up | SplitDir::Down => {
                if area.h <= 1 {
                    return false;
                }
                new_area.h = area.h / 2;
                if dir == SplitDir::Down {
                    new_area.y += new_area.h;
                }
            }
        }
        s.areas.truncate(s.current + 1);
        s.areas.push(new_area);
        s.current += 1;
        self.move_to_center(new_area);
        true
    }

    fn split_key(&mut self, cur: usize, sym: Keysym, text: &str, src: &mut dyn AreaSource) -> bool {
        if sym == Keysym::Escape {
            self.running = false;
            return false;
        }
        if sym == Keysym::Return || sym == Keysym::space {
            let a = { let s = self.hist(cur); s.areas[s.current] };
            self.enter_next_mode(a, src);
            return true;
        }
        if sym == Keysym::BackSpace {
            let s = self.hist(cur);
            if s.current > 0 {
                s.current -= 1;
                let a = s.areas[s.current];
                self.move_to_center(a);
            } else {
                self.reenter_prev_mode();
            }
            return true;
        }
        let dir = match sym {
            Keysym::Left => Some(SplitDir::Left),
            Keysym::Right => Some(SplitDir::Right),
            Keysym::Up => Some(SplitDir::Up),
            Keysym::Down => Some(SplitDir::Down),
            _ => None,
        };
        if let Some(d) = dir {
            return self.split(cur, d);
        }
        if let Some(matched) = self.find_home_row_key(text) {
            if let Some(c) = self.click_key(matched) {
                self.click = c;
                let a = { let s = self.hist(cur); s.areas[s.current] };
                self.enter_next_mode(a, src);
                return false;
            }
        }
        // `wasd` and `hjkl`, but only after the home row keys.
        match text.chars().next() {
            Some('a' | 'h') => self.split(cur, SplitDir::Left),
            Some('d' | 'l') => self.split(cur, SplitDir::Right),
            Some('w' | 'k') => self.split(cur, SplitDir::Up),
            Some('s' | 'j') => self.split(cur, SplitDir::Down),
            _ => false,
        }
    }

    // ---------------------------------------------------------------- render

    pub fn render(&self, cv: &mut Canvas, fonts: &mut Fonts) {
        let Some(cur) = self.current else { return };
        if cur >= self.kinds.len() {
            return;
        }
        match self.states[cur].as_ref() {
            Some(ModeState::Tile(s)) => self.render_tile(s, cv, fonts),
            Some(ModeState::Floating(s)) => self.render_floating(s, cv, fonts),
            Some(ModeState::Bisect(s)) => self.render_bisect(s, cv, fonts),
            Some(ModeState::Split(s)) => self.render_split(s, cv),
            _ => {}
        }
        self.render_drag_mark(cv);
    }

    /// Fonts the current chain needs, to load them before the first frame.
    pub fn font_specs(&self) -> Vec<String> {
        self.kinds
            .iter()
            .filter_map(|k| match k {
                ModeKind::Tile => Some(self.cfg.tile.label_font_family.clone()),
                ModeKind::Floating => Some(self.cfg.floating.labels.label_font_family.clone()),
                ModeKind::Bisect => Some(self.cfg.bisect.label_font_family.clone()),
                _ => None,
            })
            .collect()
    }

    fn draw_label(
        cv: &mut Canvas,
        fonts: &mut Fonts,
        cfg: &Labels,
        font_size: f64,
        sel: &LabelSelection,
        typed: &LabelSelection,
        symbols: &LabelSymbols,
        (x, y, w, h): (f64, f64, f64, f64),
        op: Op,
    ) {
        let font = &cfg.label_font_family;
        let all = sel.to_string(symbols);
        let te_all = fonts.extents(font, font_size, &all);
        let (selected, unselected) = sel.split(symbols, typed.next());
        let te_sel = fonts.extents(font, font_size, &selected);
        let te_unsel = fonts.extents(font, font_size, &unselected);
        let tx = x + (w - te_sel.x_advance - te_unsel.x_advance) / 2.0;
        let ty = y + ((h + te_all.height) / 2.0).trunc();
        let pen = cv.text(fonts, font, font_size, tx, ty, &selected, cfg.label_select_color, op);
        cv.text(fonts, font, font_size, pen, ty, &unselected, cfg.label_color, op);
    }

    fn render_tile(&self, s: &TileState, cv: &mut Canvas, fonts: &mut Fonts) {
        let cfg = &self.cfg.tile;
        let font_size = cfg.label_font_size.compute(s.sub_h as f64);
        cv.paint(cfg.unselectable_bg_color);
        let (ax, ay) = (s.area.x as f64, s.area.y as f64);
        cv.stroke_rect(ax + 0.5, ay + 0.5, s.area.w as f64 - 1.0, s.area.h as f64 - 1.0, 1.0, cfg.unselectable_bg_color, Op::Source);

        let mut cur = LabelSelection::new(&s.symbols, (s.cols * s.rows) as usize);
        cur.set_from_idx(0);
        for i in 0..s.cols {
            for j in 0..s.rows {
                let x = (i * s.sub_w + i.min(s.w_off)) as f64 + ax;
                let w = (s.sub_w + i32::from(i < s.w_off)) as f64;
                let y = (j * s.sub_h + j.min(s.h_off)) as f64 + ay;
                let h = (s.sub_h + i32::from(j < s.h_off)) as f64;
                if cur.is_included(&s.selection) {
                    cv.fill_rect(x, y, w, h, cfg.selectable_bg_color, Op::Source);
                    cv.stroke_rect(x + 0.5, y + 0.5, w - 1.0, h - 1.0, 1.0, cfg.selectable_border_color, Op::Source);
                    Self::draw_label(cv, fonts, cfg, font_size, &cur, &s.selection, &s.symbols, (x, y, w, h), Op::Source);
                }
                cur.incr();
            }
        }
    }

    fn render_floating(&self, s: &FloatingState, cv: &mut Canvas, fonts: &mut Fonts) {
        let cfg = &self.cfg.floating.labels;
        cv.paint(cfg.unselectable_bg_color);
        let mut cur = LabelSelection::new(&s.symbols, s.areas.len());
        cur.set_from_idx(0);
        for a in &s.areas {
            if cur.is_included(&s.selection) {
                cv.fill_rect(a.x as f64, a.y as f64, a.w as f64, a.h as f64, 0, Op::Source);
            }
            cur.incr();
        }
        cur.set_from_idx(0);
        for a in &s.areas {
            if cur.is_included(&s.selection) {
                let (x, y, w, h) = (a.x as f64, a.y as f64, a.w as f64, a.h as f64);
                cv.fill_rect(x, y, w, h, cfg.selectable_bg_color, Op::Over);
                cv.stroke_rect(x + 0.5, y + 0.5, w - 1.0, h - 1.0, 1.0, cfg.selectable_border_color, Op::Source);
                let font_size = cfg.label_font_size.compute(h);
                Self::draw_label(cv, fonts, cfg, font_size, &cur, &s.selection, &s.symbols, (x, y, w, h), Op::Over);
            }
            cur.incr();
        }
    }

    fn crosshair(cv: &mut Canvas, area: &Rect, size: f64, color: u32) {
        let px = (area.x + area.w / 2) as f64;
        let py = (area.y + area.h / 2) as f64;
        let half = (size / 2.0).trunc();
        cv.line(px + 0.5, py - half + 0.5, px + 0.5, py + half + 0.5, 1.0, color, Op::Source);
        cv.line(px - half + 0.5, py + 0.5, px + half + 0.5, py + 0.5, 1.0, color, Op::Source);
    }

    fn render_bisect(&self, s: &HistState, cv: &mut Canvas, fonts: &mut Fonts) {
        let cfg = &self.cfg.bisect;
        let area = s.areas[s.current];
        cv.paint(cfg.unselectable_bg_color);
        for a in &s.areas[..s.current] {
            cv.stroke_rect(a.x as f64 + 0.5, a.y as f64 + 0.5, a.w as f64 - 1.0, a.h as f64 - 1.0, 1.0, cfg.history_border_color, Op::Source);
        }
        if s.current < BISECT_MAX_HISTORY {
            let division = determine_division(&area);
            self.render_division(division, &area, cv, fonts);
        }
        Self::crosshair(cv, &area, cfg.pointer_size, cfg.pointer_color);
    }

    fn render_division(&self, division: Division, area: &Rect, cv: &mut Canvas, fonts: &mut Fonts) {
        let cfg = &self.cfg.bisect;
        let font = cfg.label_font_family.as_str();
        let hr = &self.home_row;
        let (ax, ay, aw, ah) = (area.x as f64, area.y as f64, area.w as f64, area.h as f64);
        match division {
            Division::Eight | Division::Four => {
                let divide_8 = division == Division::Eight;
                let cols = if divide_8 { 4 } else { 2 };
                let rows = 2;
                let (sw, swo) = (area.w / cols, area.w % cols);
                let (sh, sho) = (area.h / rows, area.h % rows);
                let inner_labels = cfg.label_font_size < sh as f64;
                let font_size = (512.0f64.min(sh as f64 * 0.9) as i32).max(cfg.label_font_size as i32) as f64;
                for i in 0..cols {
                    for j in 0..rows {
                        let x = (area.x + i * sw + i.min(swo)) as f64;
                        let y = (area.y + j * sh + j.min(sho)) as f64;
                        let w = (sw + i32::from(i < swo)) as f64;
                        let h = (sh + i32::from(j < sho)) as f64;
                        let even = (i + j) % 2 == 0;
                        cv.fill_rect(x, y, w, h, if even { cfg.even_area_bg_color } else { cfg.odd_area_bg_color }, Op::Source);
                        cv.stroke_rect(x + 0.5, y + 0.5, w - 1.0, h - 1.0, 1.0, if even { cfg.even_area_border_color } else { cfg.odd_area_border_color }, Op::Source);
                        if inner_labels {
                            let label = &hr[(i + j * cols) as usize];
                            let te = fonts.extents(font, font_size, label);
                            cv.text(fonts, font, font_size, x + (w / 2.0).trunc() - (te.width / 2.0).trunc(), y + (h + te.height) / 2.0, label, cfg.label_color, Op::Source);
                        }
                    }
                }
                if !inner_labels {
                    let size = cfg.label_font_size;
                    let (top, bottom) = if divide_8 {
                        (format!("{} {} {} {}", hr[0], hr[1], hr[2], hr[3]), format!("{} {} {} {}", hr[4], hr[5], hr[6], hr[7]))
                    } else {
                        (format!("{} {}", hr[0], hr[1]), format!("{} {}", hr[2], hr[3]))
                    };
                    let te = fonts.extents(font, size, &top);
                    cv.text(fonts, font, size, ax + (aw / 2.0).trunc() - (te.width / 2.0).trunc(), ay - cfg.label_padding, &top, cfg.label_color, Op::Source);
                    let te = fonts.extents(font, size, &bottom);
                    cv.text(fonts, font, size, ax + (aw / 2.0).trunc() - (te.width / 2.0).trunc(), ay + ah + te.height + cfg.label_padding, &bottom, cfg.label_color, Op::Source);
                }
            }
            Division::Horizontal => {
                let mid = (aw / 2.0).trunc();
                cv.line(ax + 0.5, ay + 0.5, ax + mid - 0.5, ay + 0.5, 1.0, cfg.even_area_border_color, Op::Source);
                cv.line(ax + mid + 0.5, ay + 0.5, ax + aw + 0.5, ay + 0.5, 1.0, cfg.odd_area_border_color, Op::Source);
                let size = cfg.label_font_size;
                let te = fonts.extents(font, size, &hr[0]);
                cv.text(fonts, font, size, ax - cfg.label_padding - te.width, ay + te.height / 2.0, &hr[0], cfg.label_color, Op::Source);
                let te = fonts.extents(font, size, &hr[1]);
                cv.text(fonts, font, size, ax + aw + cfg.label_padding, ay + te.height / 2.0, &hr[1], cfg.label_color, Op::Source);
            }
            Division::Vertical => {
                let mid = (ah / 2.0).trunc();
                cv.line(ax + 0.5, ay + 0.5, ax + 0.5, ay - 0.5 + mid, 1.0, cfg.even_area_border_color, Op::Source);
                cv.line(ax + 0.5, ay + 0.5 + mid, ax + 0.5, ay + 0.5 + ah, 1.0, cfg.odd_area_border_color, Op::Source);
                let size = cfg.label_font_size;
                let te = fonts.extents(font, size, &hr[0]);
                cv.text(fonts, font, size, ax - te.width / 2.0, ay - cfg.label_padding, &hr[0], cfg.label_color, Op::Source);
                let te = fonts.extents(font, size, &hr[1]);
                cv.text(fonts, font, size, ax - te.width / 2.0, ay + ah + cfg.label_padding + te.height, &hr[1], cfg.label_color, Op::Source);
            }
            Division::Undividable => {
                cv.circle_stroke(ax + 0.5, ay + 0.5, cfg.pointer_size / 4.0, 1.0, cfg.pointer_color, Op::Source);
            }
        }
    }

    fn render_split(&self, s: &HistState, cv: &mut Canvas) {
        let cfg = &self.cfg.split;
        cv.paint(cfg.bg_color);
        for a in &s.areas[..=s.current] {
            cv.stroke_rect(a.x as f64 + 0.5, a.y as f64 + 0.5, a.w as f64 - 1.0, a.h as f64 - 1.0, 1.0, cfg.history_border_color, Op::Source);
        }
        let area = s.areas[s.current];
        let (ax, ay, aw, ah) = (area.x as f64, area.y as f64, area.w as f64, area.h as f64);
        cv.fill_rect(ax + 0.5, ay + 0.5, aw - 1.0, ah - 1.0, cfg.area_bg_color, Op::Source);

        // Split markers: lines through the middle and arrows pointing at
        // each half.
        let mx = (area.x + area.w / 2) as f64;
        let my = (area.y + area.h / 2) as f64;
        cv.line(mx + 0.5, ay + 0.5, mx + 0.5, ay + ah + 0.5, 1.0, cfg.horizontal_color, Op::Source);
        cv.line(ax + 0.5, my + 0.5, ax + aw + 0.5, my + 0.5, 1.0, cfg.vertical_color, Op::Source);
        let arrow = |cv: &mut Canvas, x: i32, y: i32, d: [(f64, f64); 2], c: u32| {
            let (x, y) = (x as f64, y as f64);
            cv.polyline(
                &[(x + ARROW_SIZE * d[0].0 + 0.5, y + ARROW_SIZE * d[0].1 + 0.5), (x + 0.5, y + 0.5), (x + ARROW_SIZE * d[1].0 + 0.5, y + ARROW_SIZE * d[1].1 + 0.5)],
                2.0, c, Op::Source, LineCap::Butt, LineJoin::Round,
            );
        };
        if area.w > 1 {
            arrow(cv, area.x + area.w / 4, area.y + area.h / 2, [(1.0, 1.0), (1.0, -1.0)], cfg.horizontal_color);
            arrow(cv, area.x + area.w * 3 / 4, area.y + area.h / 2, [(-1.0, 1.0), (-1.0, -1.0)], cfg.horizontal_color);
        }
        if area.h > 1 {
            arrow(cv, area.x + area.w / 2, area.y + area.h / 4, [(-1.0, 1.0), (1.0, 1.0)], cfg.vertical_color);
            arrow(cv, area.x + area.w / 2, area.y + area.h * 3 / 4, [(-1.0, -1.0), (1.0, -1.0)], cfg.vertical_color);
        }

        Self::crosshair(cv, &area, cfg.pointer_size, cfg.pointer_color);
        if area.w <= 1 && area.h <= 1 {
            cv.circle_stroke(ax + 0.5, ay + 0.5, cfg.pointer_size / 4.0, 1.0, cfg.pointer_color, Op::Source);
        }
    }

    fn render_drag_mark(&self, cv: &mut Canvas) {
        if self.drag_phase == 0 {
            return;
        }
        let d = &self.cfg.drag;
        let s = d.start_marker_size;
        let (x, y) = (self.drag_start.0 as f64, self.drag_start.1 as f64);
        let c = d.start_marker_color;
        let bar_w = if s / 4.0 < 1.5 { 1.5 } else { s / 4.0 };
        let bar_h = s * 2.0;
        match d.start_marker_shape {
            MarkerShape::Circle => cv.circle_fill(x, y, s, c, Op::Over),
            MarkerShape::Rectangle => {
                cv.polyline(&[(x, y - bar_h / 2.0), (x, y + bar_h / 2.0)], bar_w, c, Op::Over, LineCap::Square, LineJoin::Miter);
            }
            MarkerShape::Caret => {
                let serif_w = s * 0.75;
                for seg in [
                    [(x, y - bar_h / 2.0), (x, y + bar_h / 2.0)],
                    [(x - serif_w / 2.0, y - bar_h / 2.0), (x + serif_w / 2.0, y - bar_h / 2.0)],
                    [(x - serif_w / 2.0, y + bar_h / 2.0), (x + serif_w / 2.0, y + bar_h / 2.0)],
                ] {
                    cv.polyline(&seg, bar_w, c, Op::Over, LineCap::Square, LineJoin::Miter);
                }
            }
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum SplitDir {
    Down,
    Up,
    Left,
    Right,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Division {
    Eight,
    Four,
    Horizontal,
    Vertical,
    Undividable,
}

fn determine_division(area: &Rect) -> Division {
    if area.w <= 1 && area.h <= 1 {
        return Division::Undividable;
    }
    if area.w <= 1 {
        return Division::Vertical;
    }
    if area.h <= 1 {
        return Division::Horizontal;
    }
    if area.w as f64 > area.h as f64 * DIVIDE_8_RATIO {
        Division::Eight
    } else {
        Division::Four
    }
}

impl Division {
    fn idx_to_sub_area(self, idx: usize, area: &Rect) -> Option<Rect> {
        let idx = idx as i32;
        match self {
            Division::Eight | Division::Four => {
                let divide_8 = self == Division::Eight;
                if !divide_8 && idx >= 4 {
                    return None;
                }
                let cols = if divide_8 { 4 } else { 2 };
                let rows = 2;
                let (sw, swo) = (area.w / cols, area.w % cols);
                let (sh, sho) = (area.h / rows, area.h % rows);
                let (i, j) = (idx % cols, idx / cols);
                Some(Rect {
                    x: area.x + i * sw + i.min(swo),
                    y: area.y + j * sh + j.min(sho),
                    w: sw + i32::from(i < swo),
                    h: sh + i32::from(j < sho),
                })
            }
            Division::Horizontal => (idx <= 1).then(|| Rect { x: area.x + idx * area.w / 2, y: area.y, w: area.w / 2, h: area.h }),
            Division::Vertical => (idx <= 1).then(|| Rect { x: area.x, y: area.y + idx * area.h / 2, w: area.w, h: area.h / 2 }),
            Division::Undividable => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Theme;

    struct NoAreas;
    impl AreaSource for NoAreas {
        fn areas(&mut self, _: FloatingSource, _: Rect) -> Vec<Rect> {
            vec![Rect { x: 10, y: 20, w: 30, h: 40 }, Rect { x: 100, y: 200, w: 50, h: 10 }]
        }
    }

    fn engine(modes: &str, drag: bool) -> Engine {
        let mut cfg = PointerConfig::new(&Theme::default());
        cfg.general.modes = modes.to_string();
        let hr = "asdfjklmghb".chars().map(|c| c.to_string()).collect();
        Engine::new(cfg, hr, drag, false).unwrap()
    }

    const SCREEN: Rect = Rect { x: 0, y: 0, w: 1920, h: 1080 };

    fn key(e: &mut Engine, sym: Keysym, text: &str) {
        e.key(sym, text, &mut NoAreas);
    }

    #[test]
    fn split_left_then_enter_picks_left_half() {
        let mut e = engine("split", false);
        e.enter_next_mode(SCREEN, &mut NoAreas);
        key(&mut e, Keysym::Left, "");
        key(&mut e, Keysym::Return, "\r");
        assert_eq!(e.result, Some(Rect { x: 0, y: 0, w: 960, h: 1080 }));
        assert_eq!(e.click, Click::None);
    }

    #[test]
    fn shift_g_double_clicks_left() {
        let mut e = engine("split", false);
        e.enter_next_mode(SCREEN, &mut NoAreas);
        key(&mut e, Keysym::G, "G");
        assert_eq!(e.click, Click::Left);
        assert!(e.double_click);
        assert!(e.has_last_mode_returned());
    }

    #[test]
    fn floating_click_uses_configured_button() {
        let mut e = engine("floating,click", false);
        e.cfg.click_button = Click::Middle;
        e.enter_next_mode(SCREEN, &mut NoAreas);
        key(&mut e, Keysym::b, "b");
        assert_eq!(e.result, Some(Rect { x: 100, y: 200, w: 50, h: 10 }));
        assert_eq!(e.click, Click::Middle);
    }

    #[test]
    fn drag_floating_left_edge_to_right_edge_with_button() {
        let mut e = engine("floating,click", true);
        e.cfg.click_button = Click::Right;
        e.enter_next_mode(SCREEN, &mut NoAreas);
        key(&mut e, Keysym::a, "a");
        assert_eq!(e.drag_start, (10, 40));
        assert!(!e.has_last_mode_returned());
        key(&mut e, Keysym::b, "b");
        assert_eq!(e.click, Click::Drag);
        assert_eq!(e.drag_button, Click::Right);
        assert_eq!(e.result, Some(Rect { x: 149, y: 205, w: 1, h: 1 }));
    }

    #[test]
    fn tile_grid_matches_wl_kbptr() {
        let mut e = engine("tile", false);
        e.enter_next_mode(SCREEN, &mut NoAreas);
        // 1920x1080 / 676 = 3067 → rows of 39px (27 rows), columns of 78px (24 cols).
        key(&mut e, Keysym::b, "b");
        key(&mut e, Keysym::a, "a");
        // idx = 1 → column 0, row 1.
        assert_eq!(e.result, Some(Rect { x: 0, y: 40, w: 80, h: 40 }));
    }

    #[test]
    fn bisect_divides_in_eight_then_four() {
        let mut e = engine("bisect", false);
        e.enter_next_mode(Rect { x: 0, y: 0, w: 400, h: 100 }, &mut NoAreas);
        key(&mut e, Keysym::f, "f"); // top row, 4th column
        assert_eq!(e.pending_move, Some((350, 25)));
        key(&mut e, Keysym::Return, "\r");
        assert_eq!(e.result, Some(Rect { x: 300, y: 0, w: 100, h: 50 }));
    }
}
