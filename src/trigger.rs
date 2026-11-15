//! `trigger = <keys> : <keeby point args>` lines: keeby runs the pointer when
//! it sees those keys, no compositor bind needed. The keys still reach the
//! focused app, so pick a combo nothing else uses.

use evdev::Key;

use crate::input::InputMsg;
use crate::labels::{is_modifier, modifier_order, parse_key_name};

struct Trigger {
    /// Ctrl, Shift, Alt, Super as bits 0..3, either side.
    mods: u8,
    key: Key,
    args: Vec<String>,
}

pub struct Triggers {
    list: Vec<Trigger>,
    held: Vec<Key>,
}

fn parse(line: &str) -> Result<Trigger, String> {
    let (keys, args) = line.split_once(':').ok_or("should be `keys : keeby point args`")?;
    let (mut mods, mut key) = (0u8, None);
    for name in keys.split('+') {
        let k = parse_key_name(name).ok_or_else(|| format!("unknown key '{}'", name.trim()))?;
        if is_modifier(k) {
            mods |= 1 << modifier_order(k);
        } else if key.replace(k).is_some() {
            return Err("only one key besides the modifiers".into());
        }
    }
    let key = key.ok_or("needs a key besides the modifiers")?;
    Ok(Trigger { mods, key, args: args.split_whitespace().map(String::from).collect() })
}

impl Triggers {
    pub fn new(lines: &[String]) -> Self {
        let list = lines
            .iter()
            .filter_map(|l| parse(l).map_err(|e| eprintln!("keeby: trigger '{l}': {e}")).ok())
            .collect();
        Self { list, held: Vec::new() }
    }

    /// Follows the held modifiers; on a key press that matches a trigger,
    /// returns its `keeby point` arguments.
    pub fn on(&mut self, msg: &InputMsg) -> Option<&[String]> {
        match *msg {
            InputMsg::Press(k) if is_modifier(k) => {
                if !self.held.contains(&k) {
                    self.held.push(k);
                }
                None
            }
            InputMsg::Release(k) => {
                self.held.retain(|h| *h != k);
                None
            }
            InputMsg::Press(k) => {
                let mods = self.held.iter().fold(0u8, |m, h| m | 1 << modifier_order(*h));
                self.list.iter().find(|t| t.key == k && t.mods == mods).map(|t| t.args.as_slice())
            }
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn triggers(lines: &[&str]) -> Triggers {
        Triggers::new(&lines.iter().map(|s| s.to_string()).collect::<Vec<_>>())
    }

    #[test]
    fn fires_only_with_exactly_its_modifiers() {
        let mut t = triggers(&["ALT+KP_4 : -o modes=floating,click -o mode_click.button=left"]);
        assert!(t.on(&InputMsg::Press(Key::KEY_KP4)).is_none());
        t.on(&InputMsg::Press(Key::KEY_RIGHTALT));
        assert_eq!(t.on(&InputMsg::Press(Key::KEY_KP4)).unwrap(), ["-o", "modes=floating,click", "-o", "mode_click.button=left"]);
        t.on(&InputMsg::Press(Key::KEY_LEFTSHIFT));
        assert!(t.on(&InputMsg::Press(Key::KEY_KP4)).is_none());
        t.on(&InputMsg::Release(Key::KEY_LEFTSHIFT));
        t.on(&InputMsg::Release(Key::KEY_RIGHTALT));
        assert!(t.on(&InputMsg::Press(Key::KEY_KP4)).is_none());
    }

    #[test]
    fn picks_the_trigger_for_the_held_modifiers() {
        let mut t = triggers(&["ALT+KP_4 : -o modes=floating,click", "SHIFT+ALT+KP_4 : -d -o modes=floating,click"]);
        t.on(&InputMsg::Press(Key::KEY_LEFTALT));
        t.on(&InputMsg::Press(Key::KEY_LEFTSHIFT));
        assert_eq!(t.on(&InputMsg::Press(Key::KEY_KP4)).unwrap()[0], "-d");
    }

    #[test]
    fn bad_lines_are_skipped() {
        let t = triggers(&["ALT+KP_4", "ALT+NOPE : x", "ALT : x", "A+B : x", "Ctrl+Alt+P : -o modes=split"]);
        assert_eq!(t.list.len(), 1);
    }
}
