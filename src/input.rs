//! Keys → the bytes a program reads from its terminal (xterm conventions).
//!
//! Pure functions over a small key description, so every sequence is tested
//! without a window.

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Mods {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
}

impl Mods {
    /// xterm's modifier parameter: 1 + shift + 2·alt + 4·ctrl
    fn param(self) -> u8 {
        1 + self.shift as u8 + 2 * self.alt as u8 + 4 * self.ctrl as u8
    }
    fn any(self) -> bool {
        self.ctrl || self.alt || self.shift
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Named {
    Enter,
    Tab,
    Backspace,
    Escape,
    Up,
    Down,
    Left,
    Right,
    Home,
    End,
    PageUp,
    PageDown,
    Insert,
    Delete,
    F(u8),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Key<'a> {
    Named(Named),
    /// the text the key produced (already shifted, layout applied)
    Text(&'a str),
}

/// `app_cursor`: the program asked for application cursor keys (DECCKM).
pub fn encode(key: &Key, mods: Mods, app_cursor: bool) -> Option<Vec<u8>> {
    match key {
        Key::Text(t) => text(t, mods),
        Key::Named(n) => named(*n, mods, app_cursor),
    }
}

fn text(t: &str, mods: Mods) -> Option<Vec<u8>> {
    if t.is_empty() {
        return None;
    }
    let mut out = Vec::new();
    let mut chars = t.chars();
    let first = chars.next().unwrap();
    if mods.ctrl && chars.next().is_none() {
        // Ctrl + a key: the C0 control code
        let c = first.to_ascii_lowercase();
        let code = match c {
            'a'..='z' => Some(c as u8 - b'a' + 1),
            '@' | ' ' | '2' => Some(0),
            '[' | '3' => Some(0x1b),
            '\\' | '4' => Some(0x1c),
            ']' | '5' => Some(0x1d),
            '^' | '6' => Some(0x1e),
            '_' | '-' | '7' => Some(0x1f),
            '?' | '8' => Some(0x7f),
            _ => None,
        };
        if let Some(code) = code {
            if mods.alt {
                out.push(0x1b);
            }
            out.push(code);
            return Some(out);
        }
    }
    if mods.alt {
        out.push(0x1b);
    }
    out.extend_from_slice(t.as_bytes());
    Some(out)
}

fn named(n: Named, mods: Mods, app_cursor: bool) -> Option<Vec<u8>> {
    let csi = |s: &str| Some(format!("\x1b[{s}").into_bytes());
    // keys with a CSI letter: arrows, Home, End (and F1–F4 as SS3 letters)
    let letter = |l: char| -> Option<Vec<u8>> {
        if mods.any() {
            csi(&format!("1;{}{l}", mods.param()))
        } else if app_cursor {
            Some(format!("\x1bO{l}").into_bytes())
        } else {
            csi(&l.to_string())
        }
    };
    // keys with a CSI number and ~: PageUp, Delete, F5…
    let tilde = |num: u8| -> Option<Vec<u8>> {
        if mods.any() { csi(&format!("{num};{}~", mods.param())) } else { csi(&format!("{num}~")) }
    };
    match n {
        Named::Enter => Some(if mods.alt { b"\x1b\r".to_vec() } else { b"\r".to_vec() }),
        Named::Tab => Some(if mods.shift { b"\x1b[Z".to_vec() } else { b"\t".to_vec() }),
        Named::Backspace => Some(match (mods.ctrl, mods.alt) {
            (true, _) => vec![0x08],
            (false, true) => vec![0x1b, 0x7f],
            _ => vec![0x7f],
        }),
        Named::Escape => Some(vec![0x1b]),
        Named::Up => letter('A'),
        Named::Down => letter('B'),
        Named::Right => letter('C'),
        Named::Left => letter('D'),
        Named::Home => letter('H'),
        Named::End => letter('F'),
        Named::Insert => tilde(2),
        Named::Delete => tilde(3),
        Named::PageUp => tilde(5),
        Named::PageDown => tilde(6),
        Named::F(k @ 1..=4) => {
            let l = [b'P', b'Q', b'R', b'S'][(k - 1) as usize] as char;
            if mods.any() { csi(&format!("1;{}{l}", mods.param())) } else { Some(format!("\x1bO{l}").into_bytes()) }
        }
        Named::F(k @ 5..=12) => tilde([15, 17, 18, 19, 20, 21, 23, 24][(k - 5) as usize]),
        Named::F(_) => None,
    }
}

/// Keys in Vim-like notation (`echo hi<CR>`, `<C-a>`, `<Up>`, `<S-Tab>`,
/// `<M-b>`), encoded exactly as the window would: for headless tests.
pub fn encode_notation(s: &str, app_cursor: bool) -> Vec<Vec<u8>> {
    let mut out = Vec::new();
    let chars: Vec<char> = s.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '<' {
            if let Some(end) = chars[i + 1..].iter().position(|c| *c == '>') {
                let name: String = chars[i + 1..i + 1 + end].iter().collect();
                let (mods_s, base) = name.rsplit_once('-').map(|(m, b)| (m.to_string(), b.to_string())).unwrap_or_default();
                let (mods_s, base) = if base.is_empty() { (String::new(), name.clone()) } else { (mods_s, base) };
                let mods = Mods { ctrl: mods_s.contains('C'), alt: mods_s.contains('M') || mods_s.contains('A'), shift: mods_s.contains('S') };
                let key = match base.to_ascii_lowercase().as_str() {
                    "cr" | "enter" => Some(Key::Named(Named::Enter)),
                    "tab" => Some(Key::Named(Named::Tab)),
                    "bs" => Some(Key::Named(Named::Backspace)),
                    "esc" => Some(Key::Named(Named::Escape)),
                    "up" => Some(Key::Named(Named::Up)),
                    "down" => Some(Key::Named(Named::Down)),
                    "left" => Some(Key::Named(Named::Left)),
                    "right" => Some(Key::Named(Named::Right)),
                    "home" => Some(Key::Named(Named::Home)),
                    "end" => Some(Key::Named(Named::End)),
                    "del" => Some(Key::Named(Named::Delete)),
                    "pageup" => Some(Key::Named(Named::PageUp)),
                    "pagedown" => Some(Key::Named(Named::PageDown)),
                    _ => None,
                };
                let bytes = match key {
                    Some(k) => encode(&k, mods, app_cursor),
                    None if base.chars().count() == 1 => {
                        let b = base.clone();
                        encode(&Key::Text(&b), mods, app_cursor)
                    }
                    None => None,
                };
                if let Some(b) = bytes {
                    out.push(b);
                    i += end + 2;
                    continue;
                }
            }
        }
        let mut buf = [0u8; 4];
        out.push(chars[i].encode_utf8(&mut buf).as_bytes().to_vec());
        i += 1;
    }
    out
}

/// Pasted text: bracketed when the program asked for it (it then knows the
/// text was pasted, not typed), with any embedded end-marker removed so a
/// paste cannot break out of the brackets.
pub fn paste(text: &str, bracketed: bool) -> Vec<u8> {
    if bracketed {
        let clean = text.replace("\x1b[201~", "");
        format!("\x1b[200~{clean}\x1b[201~").into_bytes()
    } else {
        // a terminal sends Enter as CR; pasted LFs become CRs like typed ones
        text.replace("\r\n", "\r").replace('\n', "\r").into_bytes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NONE: Mods = Mods { ctrl: false, alt: false, shift: false };
    const CTRL: Mods = Mods { ctrl: true, alt: false, shift: false };
    const ALT: Mods = Mods { ctrl: false, alt: true, shift: false };
    const SHIFT: Mods = Mods { ctrl: false, alt: false, shift: true };

    fn e(k: Key, m: Mods, app: bool) -> Vec<u8> {
        encode(&k, m, app).unwrap()
    }

    #[test]
    fn plain_text() {
        assert_eq!(e(Key::Text("a"), NONE, false), b"a");
        assert_eq!(e(Key::Text("ü"), NONE, false), "ü".as_bytes());
        assert_eq!(e(Key::Text("x"), ALT, false), b"\x1bx");
    }

    #[test]
    fn control_codes() {
        assert_eq!(e(Key::Text("c"), CTRL, false), [3]);
        assert_eq!(e(Key::Text("C"), CTRL, false), [3]);
        assert_eq!(e(Key::Text("["), CTRL, false), [0x1b]);
        assert_eq!(e(Key::Text(" "), CTRL, false), [0]);
        assert_eq!(e(Key::Text("r"), Mods { ctrl: true, alt: true, shift: false }, false), [0x1b, 18]);
    }

    #[test]
    fn arrows() {
        assert_eq!(e(Key::Named(Named::Up), NONE, false), b"\x1b[A");
        assert_eq!(e(Key::Named(Named::Up), NONE, true), b"\x1bOA", "application cursor mode");
        assert_eq!(e(Key::Named(Named::Left), CTRL, true), b"\x1b[1;5D", "modifiers win over app mode");
        assert_eq!(e(Key::Named(Named::Right), SHIFT, false), b"\x1b[1;2C");
    }

    #[test]
    fn editing_and_function_keys() {
        assert_eq!(e(Key::Named(Named::Enter), NONE, false), b"\r");
        assert_eq!(e(Key::Named(Named::Backspace), NONE, false), [0x7f]);
        assert_eq!(e(Key::Named(Named::Tab), SHIFT, false), b"\x1b[Z");
        assert_eq!(e(Key::Named(Named::Delete), NONE, false), b"\x1b[3~");
        assert_eq!(e(Key::Named(Named::PageUp), CTRL, false), b"\x1b[5;5~");
        assert_eq!(e(Key::Named(Named::F(1)), NONE, false), b"\x1bOP");
        assert_eq!(e(Key::Named(Named::F(5)), NONE, false), b"\x1b[15~");
        assert_eq!(e(Key::Named(Named::F(12)), NONE, false), b"\x1b[24~");
    }

    #[test]
    fn pasting() {
        assert_eq!(paste("a\nb", false), b"a\rb");
        assert_eq!(paste("hi", true), b"\x1b[200~hi\x1b[201~");
        assert_eq!(paste("x\x1b[201~rm -rf", true), b"\x1b[200~xrm -rf\x1b[201~", "a paste cannot close the brackets itself");
    }
}
