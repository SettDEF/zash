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

/// The Kitty keyboard protocol's flags a program asked for (CSI > flags u): 1 disambiguate (Esc
/// and modified keys as CSI code;mods u), 8 every key as an escape code. 0: the classic encoding.
pub const KITTY_DISAMBIGUATE: u8 = 1;
pub const KITTY_ALL_KEYS: u8 = 8;

/// As [`encode`], under the Kitty keyboard protocol's `flags` (0 is [`encode`] itself). Arrows,
/// Home/End, F-keys and the ~ keys keep their forms, which the protocol shares.
pub fn encode_kitty(key: &Key, mods: Mods, app_cursor: bool, flags: u8) -> Option<Vec<u8>> {
    if flags & (KITTY_DISAMBIGUATE | KITTY_ALL_KEYS) == 0 {
        return encode(key, mods, app_cursor);
    }
    let all = flags & KITTY_ALL_KEYS != 0;
    let csi_u = |code: u32| -> Vec<u8> {
        if mods.any() { format!("\x1b[{code};{}u", mods.param()) } else { format!("\x1b[{code}u") }.into_bytes()
    };
    match key {
        Key::Text(t) => {
            let mut cs = t.chars();
            let (Some(c), None) = (cs.next(), cs.next()) else { return encode(key, mods, app_cursor) };
            // Ctrl or Alt with a key (Shift alone just types its character): the key's own code
            if all || mods.ctrl || mods.alt {
                return Some(csi_u(c.to_lowercase().next().unwrap_or(c) as u32));
            }
            encode(key, mods, app_cursor)
        }
        Key::Named(n) => {
            let code = match n {
                Named::Escape => Some(27),
                Named::Enter if all || mods.any() => Some(13),
                Named::Tab if all || mods.any() => Some(9),
                Named::Backspace if all || mods.any() => Some(127),
                _ => None,
            };
            match code {
                Some(c) => Some(csi_u(c)),
                None => encode(key, mods, app_cursor),
            }
        }
    }
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

/// A mouse event for a program that asked for the mouse (tmux, Neovim, htop).
/// `button`: 0 left, 1 middle, 2 right, 3 release (X10), 64/65 wheel up/down;
/// `motion` adds 32. `col`/`row` are 0-based cells. SGR (1006) encoding when
/// the program asked for it, the old X10 form otherwise.
pub fn mouse(button: u8, motion: bool, pressed: bool, col: usize, row: usize, mods: Mods, sgr: bool) -> Vec<u8> {
    let mut b = button as u32 + if motion { 32 } else { 0 };
    if mods.shift { b += 4; }
    if mods.alt { b += 8; }
    if mods.ctrl { b += 16; }
    if sgr {
        format!("\x1b[<{};{};{}{}", b, col + 1, row + 1, if pressed { 'M' } else { 'm' }).into_bytes()
    } else {
        // X10: a release is button 3; coordinates past 223 cannot be sent
        let b = if pressed || button >= 64 { b } else { 3 + (b & !3) };
        let c = |v: usize| (32 + 1 + v.min(222)) as u8;
        vec![0x1b, b'[', b'M', (32 + b) as u8, c(col), c(row)]
    }
}

/// Keys in Vim-like notation (`echo hi<CR>`, `<C-a>`, `<Up>`, `<S-Tab>`,
/// `<M-b>`), encoded exactly as the window would: for headless tests.
#[cfg(test)]
pub fn encode_notation(s: &str, app_cursor: bool) -> Vec<Vec<u8>> {
    encode_notation_kitty(s, app_cursor, 0)
}

/// As [`encode_notation`], under the Kitty keyboard protocol's flags the program asked for.
pub fn encode_notation_kitty(s: &str, app_cursor: bool, kitty: u8) -> Vec<Vec<u8>> {
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
                    "space" => Some(Key::Text(" ")),
                    _ => None,
                };
                let bytes = match key {
                    Some(k) => encode_kitty(&k, mods, app_cursor, kitty),
                    None if base.chars().count() == 1 => {
                        let b = base.clone();
                        encode_kitty(&Key::Text(&b), mods, app_cursor, kitty)
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
        let c = chars[i].encode_utf8(&mut buf);
        // a plain character: as itself, but as an escape code when the program asked for every key
        out.push(if kitty & KITTY_ALL_KEYS != 0 { encode_kitty(&Key::Text(c), Mods::default(), app_cursor, kitty).unwrap_or_default() } else { c.as_bytes().to_vec() });
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
    fn mouse_reports() {
        assert_eq!(mouse(0, false, true, 4, 9, NONE, true), b"\x1b[<0;5;10M");
        assert_eq!(mouse(0, false, false, 4, 9, NONE, true), b"\x1b[<0;5;10m");
        assert_eq!(mouse(0, true, true, 0, 0, NONE, true), b"\x1b[<32;1;1M", "drag");
        assert_eq!(mouse(64, false, true, 0, 0, NONE, true), b"\x1b[<64;1;1M", "wheel up");
        assert_eq!(mouse(0, false, true, 0, 0, CTRL, true), b"\x1b[<16;1;1M");
        assert_eq!(mouse(0, false, true, 1, 2, NONE, false), [0x1b, b'[', b'M', 32, 34, 35], "X10 press");
        assert_eq!(mouse(0, false, false, 1, 2, NONE, false), [0x1b, b'[', b'M', 35, 34, 35], "X10 release is button 3");
    }

    #[test]
    fn notation() {
        assert_eq!(encode_notation("a<CR>", false), vec![b"a".to_vec(), b"\r".to_vec()]);
        assert_eq!(encode_notation("<C-a><M-f><Up>", false), vec![vec![1], b"\x1bf".to_vec(), b"\x1b[A".to_vec()]);
        assert_eq!(encode_notation("<S-Tab>", false), vec![b"\x1b[Z".to_vec()]);
    }

    #[test]
    fn kitty_keyboard() {
        let k = |s: &str, f: u8| encode_notation_kitty(s, false, f);
        // disambiguate: Esc and modified keys as CSI code;mods u, plain typing as it was
        assert_eq!(k("<Esc>", KITTY_DISAMBIGUATE), vec![b"\x1b[27u".to_vec()]);
        assert_eq!(k("<C-a>", KITTY_DISAMBIGUATE), vec![b"\x1b[97;5u".to_vec()]);
        assert_eq!(k("<C-=>", KITTY_DISAMBIGUATE), vec![b"\x1b[61;5u".to_vec()], "C-= has no C0 code: only the protocol tells it");
        assert_eq!(k("<C-\\>", KITTY_DISAMBIGUATE), vec![b"\x1b[92;5u".to_vec()]);
        assert_eq!(k("<M-f>", KITTY_DISAMBIGUATE), vec![b"\x1b[102;3u".to_vec()]);
        assert_eq!(k("<C-CR>", KITTY_DISAMBIGUATE), vec![b"\x1b[13;5u".to_vec()]);
        assert_eq!(k("a<CR><Tab><BS>", KITTY_DISAMBIGUATE), vec![b"a".to_vec(), b"\r".to_vec(), b"\t".to_vec(), vec![0x7f]]);
        assert_eq!(k("<Up><S-Up>", KITTY_DISAMBIGUATE), vec![b"\x1b[A".to_vec(), b"\x1b[1;2A".to_vec()]);
        // every key: text too
        assert_eq!(k("a<CR>", KITTY_ALL_KEYS), vec![b"\x1b[97u".to_vec(), b"\x1b[13u".to_vec()]);
        // no flags: the classic forms
        assert_eq!(k("<C-a><Esc>", 0), vec![vec![1], vec![0x1b]]);
    }

    #[test]
    fn pasting() {
        assert_eq!(paste("a\nb", false), b"a\rb");
        assert_eq!(paste("hi", true), b"\x1b[200~hi\x1b[201~");
        assert_eq!(paste("x\x1b[201~rm -rf", true), b"\x1b[200~xrm -rf\x1b[201~", "a paste cannot close the brackets itself");
    }
}
