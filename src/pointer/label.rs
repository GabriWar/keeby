// SPDX-License-Identifier: GPL-3.0-only
// Ported from wl-kbptr (https://github.com/moverest/wl-kbptr) by moverest.
//! Label symbols and selections, ported from wl-kbptr's `label.c`.
//!
//! A label is a sequence of symbol indices. The first typed symbol is the
//! least significant digit of the label's index, exactly like wl-kbptr.

pub struct LabelSymbols {
    symbols: Vec<String>,
}

impl LabelSymbols {
    pub fn from_str(s: &str) -> Result<Self, String> {
        let symbols: Vec<String> = s.chars().map(|c| c.to_string()).collect();
        if symbols.len() < 2 {
            return Err(format!(
                "Not enough characters ({}). Must have at least 2.",
                symbols.len()
            ));
        }
        if symbols.len() >= 255 {
            return Err(format!("Too many characters ({}).", symbols.len()));
        }
        Ok(Self { symbols })
    }

    pub fn len(&self) -> usize {
        self.symbols.len()
    }

    pub fn get(&self, idx: usize) -> &str {
        &self.symbols[idx]
    }

    pub fn find_idx(&self, s: &str) -> Option<usize> {
        self.symbols.iter().position(|x| x == s)
    }

    #[cfg(test)]
    fn max_str_len(&self) -> usize {
        self.symbols.iter().map(|s| s.len()).max().unwrap_or(0)
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum AppendRet {
    Success,
    IdxOverflow,
    Full,
}

#[derive(Clone)]
pub struct LabelSelection {
    num_symbols: usize,
    num_labels: usize,
    len: usize,
    next: usize,
    input: Vec<usize>,
}

impl LabelSelection {
    pub fn new(symbols: &LabelSymbols, num_labels: usize) -> Self {
        let n = symbols.len();
        let mut len = 0;
        let mut rest = num_labels;
        while rest > 0 {
            len += 1;
            rest /= n;
        }
        Self {
            num_symbols: n,
            num_labels,
            len,
            next: 0,
            input: vec![0; n.max(len)],
        }
    }

    pub fn next(&self) -> usize {
        self.next
    }

    pub fn clear(&mut self) {
        self.next = 0;
    }

    fn partial_idx(&self) -> usize {
        let mut idx = 0;
        let mut factor = 1;
        for i in 0..self.next {
            idx += self.input[i] * factor;
            factor *= self.num_symbols;
        }
        idx
    }

    pub fn append(&mut self, idx: usize) -> AppendRet {
        // Same bound as wl-kbptr: the number of symbols, not the label length.
        if self.next >= self.num_symbols {
            return AppendRet::Full;
        }
        self.input[self.next] = idx;
        self.next += 1;
        if self.partial_idx() >= self.num_labels {
            self.next -= 1;
            return AppendRet::IdxOverflow;
        }
        AppendRet::Success
    }

    pub fn back(&mut self) -> bool {
        if self.next == 0 {
            return false;
        }
        self.next -= 1;
        true
    }

    /// True if `start` is the beginning of `self`.
    pub fn is_included(&self, start: &LabelSelection) -> bool {
        if self.num_symbols != start.num_symbols || self.len != start.len || self.next < start.next
        {
            return false;
        }
        self.input[..start.next] == start.input[..start.next]
    }

    pub fn to_idx(&self) -> Option<usize> {
        if self.next != self.len {
            return None;
        }
        Some(self.partial_idx())
    }

    pub fn set_from_idx(&mut self, mut idx: usize) -> bool {
        self.next = 0;
        while self.next < self.len {
            self.input[self.next] = idx % self.num_symbols;
            idx /= self.num_symbols;
            self.next += 1;
        }
        idx == 0
    }

    pub fn incr(&mut self) -> bool {
        for i in 0..self.len {
            self.input[i] += 1;
            if self.input[i] < self.num_symbols {
                return true;
            }
            self.input[i] %= self.num_symbols;
        }
        false
    }

    #[cfg(test)]
    pub fn str_max_len(&self, symbols: &LabelSymbols) -> usize {
        symbols.max_str_len() * self.len
    }

    pub fn to_string(&self, symbols: &LabelSymbols) -> String {
        (0..self.next).map(|i| symbols.get(self.input[i])).collect()
    }

    /// Label split at `cut` symbols into (typed prefix, rest).
    pub fn split(&self, symbols: &LabelSymbols, cut: usize) -> (String, String) {
        let cut = cut.min(self.next);
        let prefix = (0..cut).map(|i| symbols.get(self.input[i])).collect();
        let suffix = (cut..self.next).map(|i| symbols.get(self.input[i])).collect();
        (prefix, suffix)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Port of wl-kbptr's test_label.c.
    #[test]
    fn label_c_suite() {
        let ls = LabelSymbols::from_str("abcdé").unwrap();
        assert_eq!(ls.len(), 5);
        assert_eq!(ls.get(0), "a");
        for (i, s) in ["a", "b", "c", "d", "é"].iter().enumerate() {
            assert_eq!(ls.find_idx(s), Some(i));
        }

        let mut sel = LabelSelection::new(&ls, 100);
        assert_eq!(sel.str_max_len(&ls) + 1, 7);
        for i in 0..100 {
            sel.set_from_idx(i);
            assert_eq!(sel.to_idx(), Some(i));
        }

        sel.clear();
        sel.append(4);
        sel.append(2);
        assert_eq!(sel.to_string(&ls), "éc");
        sel.append(0);
        assert_eq!(sel.to_idx(), Some(14));
        assert!(sel.incr());
        assert_eq!(sel.to_idx(), Some(15));
        let (p, s) = sel.split(&ls, 1);
        assert_eq!(p, "a");
        assert_eq!(s, "da");

        let alt = LabelSymbols::from_str("abcdéfghi").unwrap();
        let alt_sel = LabelSelection::new(&alt, 100);
        assert_eq!(alt_sel.str_max_len(&alt) + 1, 7);
        for (i, s) in ["a", "b", "c", "d", "é", "f", "g", "h", "i"].iter().enumerate() {
            assert_eq!(alt.find_idx(s), Some(i));
        }
    }

    #[test]
    fn rejects_bad_symbol_sets() {
        assert!(LabelSymbols::from_str("a").is_err());
        assert!(LabelSymbols::from_str(&"x".repeat(255)).is_err());
    }

    #[test]
    fn append_overflow() {
        let ls = LabelSymbols::from_str("ab").unwrap();
        let mut sel = LabelSelection::new(&ls, 3); // len 2
        assert_eq!(sel.append(1), AppendRet::Success);
        assert_eq!(sel.append(1), AppendRet::IdxOverflow); // 1 + 2 = 3 >= 3
        assert_eq!(sel.next(), 1);
    }
}
