//! Images in the terminal: the Kitty graphics protocol (APC `G` sequences).

use std::collections::HashMap;
use std::sync::Arc;

#[derive(Clone)]
pub struct Image {
    pub w: u32,
    pub h: u32,
    pub rgba: Arc<Vec<u8>>,
}

/// An image shown at a place in the text: it scrolls with the line it was put on.
#[derive(Clone, Debug, PartialEq)]
pub struct Placement {
    pub img: u32,
    /// the line (counted from the oldest kept), and the column
    pub line: usize,
    pub col: usize,
    pub cols: usize,
    pub rows: usize,
}

#[derive(Default)]
pub struct Graphics {
    pub images: HashMap<u32, Image>,
    pub places: Vec<Placement>,
    /// a chunked transfer under way: its control keys and the base64 so far
    pending: Option<(HashMap<String, String>, String)>,
    next_auto: u32,
    /// bumped on every change, so the renderer knows when to look again
    pub generation: u64,
}

/// What a command asks of the terminal after it was handled.
#[derive(Debug, Default, PartialEq)]
pub struct Outcome {
    /// a reply for the program (`ESC _ G … ESC \`)
    pub reply: Option<String>,
    /// move the cursor past a placed image: (rows down, columns right)
    pub advance: Option<(usize, usize)>,
}

pub fn base64(s: &str) -> Vec<u8> {
    let val = |c: u8| -> Option<u32> {
        Some(match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' | b'-' => 62,
            b'/' | b'_' => 63,
            _ => return None,
        } as u32)
    };
    let mut out = Vec::with_capacity(s.len() * 3 / 4);
    let (mut acc, mut bits) = (0u32, 0);
    for c in s.bytes().filter_map(val) {
        acc = (acc << 6) | c;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    out
}

fn decode(keys: &HashMap<String, String>, data: Vec<u8>) -> Result<Image, String> {
    let num = |k: &str| keys.get(k).and_then(|v| v.parse::<u32>().ok());
    // t=f / t=t: the payload is a path to read (t=t: a temporary file, removed after)
    let data = match keys.get("t").map(String::as_str) {
        Some("f") | Some("t") => {
            let path = String::from_utf8_lossy(&data).to_string();
            let bytes = std::fs::read(&path).map_err(|e| format!("EBADF:{e}"))?;
            if keys.get("t").map(String::as_str) == Some("t") && path.contains("tty-graphics-protocol") {
                let _ = std::fs::remove_file(&path);
            }
            bytes
        }
        _ => data,
    };
    match num("f").unwrap_or(32) {
        100 => {
            let dec = png::Decoder::new(std::io::Cursor::new(data));
            let mut r = dec.read_info().map_err(|e| format!("EBADPNG:{e}"))?;
            let mut buf = vec![0; r.output_buffer_size()];
            let info = r.next_frame(&mut buf).map_err(|e| format!("EBADPNG:{e}"))?;
            let px = &buf[..info.buffer_size()];
            let rgba: Vec<u8> = match info.color_type {
                png::ColorType::Rgba => px.to_vec(),
                png::ColorType::Rgb => px.chunks(3).flat_map(|c| [c[0], c[1], c[2], 255]).collect(),
                png::ColorType::GrayscaleAlpha => px.chunks(2).flat_map(|c| [c[0], c[0], c[0], c[1]]).collect(),
                png::ColorType::Grayscale => px.iter().flat_map(|&g| [g, g, g, 255]).collect(),
                png::ColorType::Indexed => return Err("EBADPNG:indexed colours".into()),
            };
            if info.bit_depth != png::BitDepth::Eight {
                return Err("EBADPNG:only 8-bit".into());
            }
            Ok(Image { w: info.width, h: info.height, rgba: Arc::new(rgba) })
        }
        f @ (24 | 32) => {
            let (w, h) = (num("s").ok_or("EINVAL:no width")?, num("v").ok_or("EINVAL:no height")?);
            let n = (w * h) as usize;
            let rgba: Vec<u8> = if f == 32 { data } else { data.chunks(3).flat_map(|c| [c[0], c.get(1).copied().unwrap_or(0), c.get(2).copied().unwrap_or(0), 255]).collect() };
            if rgba.len() < n * 4 {
                return Err("ENODATA:short".into());
            }
            Ok(Image { w, h, rgba: Arc::new(rgba[..n * 4].to_vec()) })
        }
        other => Err(format!("EINVAL:format {other}")),
    }
}

impl Graphics {
    /// One `G` command (what follows `ESC _ G`, up to `ESC \`). `cell` is a cell's size in
    /// pixels, `at` the cursor (line counted from the oldest kept, column).
    pub fn command(&mut self, body: &str, cell: (f32, f32), at: (usize, usize)) -> Outcome {
        let (ctrl, payload) = body.split_once(';').unwrap_or((body, ""));
        let mut keys: HashMap<String, String> = ctrl.split(',').filter_map(|kv| kv.split_once('=')).map(|(k, v)| (k.to_string(), v.to_string())).collect();
        // later chunks carry only m (and maybe q): the first chunk's keys rule
        if let Some((first, mut data)) = self.pending.take() {
            data.push_str(payload);
            if keys.get("m").map(String::as_str) == Some("1") {
                self.pending = Some((first, data));
                return Outcome::default();
            }
            return self.run(first, data, cell, at);
        }
        if keys.get("m").map(String::as_str) == Some("1") {
            keys.remove("m");
            self.pending = Some((keys, payload.to_string()));
            return Outcome::default();
        }
        self.run(keys, payload.to_string(), cell, at)
    }

    fn run(&mut self, keys: HashMap<String, String>, payload: String, cell: (f32, f32), at: (usize, usize)) -> Outcome {
        let num = |k: &str| keys.get(k).and_then(|v| v.parse::<u32>().ok());
        let action = keys.get("a").map(String::as_str).unwrap_or("t");
        let quiet = num("q").unwrap_or(0);
        let mut id = num("i").unwrap_or(0);
        let reply = |id: u32, msg: &str, err: bool| -> Option<String> {
            if id == 0 || quiet == 2 || (quiet == 1 && !err) {
                return None;
            }
            Some(format!("\x1b_Gi={id};{msg}\x1b\\"))
        };
        match action {
            "q" => {
                // a query: can an image like this be shown? (nothing is kept)
                let r = decode(&keys, base64(&payload));
                return Outcome { reply: reply(id, r.as_ref().map(|_| "OK").unwrap_or_else(|e| e.as_str()), r.is_err()), advance: None };
            }
            "d" => {
                match keys.get("d").map(String::as_str).unwrap_or("a") {
                    "i" | "I" => {
                        self.places.retain(|p| p.img != id);
                        if keys.get("d").map(String::as_str) == Some("I") {
                            self.images.remove(&id);
                        }
                    }
                    _ => {
                        // a, A and the rest: every placement (A also frees the images)
                        self.places.clear();
                        if keys.get("d").map(String::as_str) == Some("A") {
                            self.images.clear();
                        }
                    }
                }
                self.generation += 1;
                return Outcome::default();
            }
            "p" => {}
            "t" | "T" => {
                if id == 0 {
                    self.next_auto += 1;
                    id = 0x7000_0000 + self.next_auto;
                }
                match decode(&keys, base64(&payload)) {
                    Ok(img) => {
                        self.images.insert(id, img);
                        self.generation += 1;
                    }
                    Err(e) => return Outcome { reply: reply(num("i").unwrap_or(0), &e, true), advance: None },
                }
                if action == "t" {
                    return Outcome { reply: reply(num("i").unwrap_or(0), "OK", false), advance: None };
                }
            }
            _ => return Outcome { reply: reply(id, "EINVAL:action", true), advance: None },
        }
        // display (a=T, or a=p for an image sent before)
        let Some(img) = self.images.get(&id) else { return Outcome { reply: reply(id, "ENOENT:no such image", true), advance: None } };
        let (cw, ch) = (cell.0.max(1.0), cell.1.max(1.0));
        let cols = num("c").map(|c| c as usize).unwrap_or_else(|| (img.w as f32 / cw).ceil() as usize).max(1);
        let rows = num("r").map(|r| r as usize).unwrap_or_else(|| (img.h as f32 / ch).ceil() as usize).max(1);
        self.places.push(Placement { img: id, line: at.0, col: at.1, cols, rows });
        // a few hundred at most: the oldest go
        if self.places.len() > 400 {
            self.places.remove(0);
        }
        self.generation += 1;
        let advance = (num("C") != Some(1)).then_some((rows.saturating_sub(1), cols));
        Outcome { reply: reply(num("i").unwrap_or(0), "OK", false), advance }
    }

    /// Images no placement uses any more are freed.
    pub fn trim(&mut self, oldest_line: usize) {
        self.places.retain(|p| p.line + p.rows > oldest_line);
        let used: std::collections::HashSet<u32> = self.places.iter().map(|p| p.img).collect();
        let before = self.images.len();
        // images sent with an id may be placed again later (a=p): keep up to 64 unplaced
        if self.images.len() > 64 {
            self.images.retain(|id, _| used.contains(id));
        }
        if self.images.len() != before {
            self.generation += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rgba_cmd(id: u32, w: u32, h: u32, extra: &str) -> String {
        let px: Vec<u8> = (0..w * h).flat_map(|_| [255u8, 0, 0, 255]).collect();
        // standard base64 of the pixels
        const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut b = String::new();
        for c in px.chunks(3) {
            let n = (c[0] as u32) << 16 | (*c.get(1).unwrap_or(&0) as u32) << 8 | *c.get(2).unwrap_or(&0) as u32;
            for i in 0..4 {
                if i <= c.len() {
                    b.push(T[(n >> (18 - 6 * i) & 63) as usize] as char);
                } else {
                    b.push('=');
                }
            }
        }
        format!("a=T,f=32,s={w},v={h},i={id}{extra};{b}")
    }

    #[test]
    fn transmit_display_and_reply() {
        let mut g = Graphics::default();
        let o = g.command(&rgba_cmd(7, 20, 30, ""), (10.0, 20.0), (5, 2));
        assert_eq!(o.reply.as_deref(), Some("\x1b_Gi=7;OK\x1b\\"));
        assert_eq!(o.advance, Some((1, 2)), "two rows tall, two columns wide");
        assert_eq!(g.places, vec![Placement { img: 7, line: 5, col: 2, cols: 2, rows: 2 }]);
        assert_eq!(g.images[&7].rgba.len(), 20 * 30 * 4);
        // C=1: the cursor stays; q=2: no reply
        let o = g.command(&rgba_cmd(8, 10, 10, ",C=1,q=2"), (10.0, 20.0), (9, 0));
        assert_eq!(o, Outcome { reply: None, advance: None });
        // delete all placements
        g.command("a=d,d=a", (10.0, 20.0), (0, 0));
        assert!(g.places.is_empty());
    }

    #[test]
    fn chunks_join() {
        let full = rgba_cmd(3, 4, 4, "");
        let (ctrl, data) = full.split_once(';').unwrap();
        let (a, b) = data.split_at(data.len() / 2);
        let mut g = Graphics::default();
        assert_eq!(g.command(&format!("{ctrl},m=1;{a}"), (10.0, 20.0), (0, 0)), Outcome::default());
        let o = g.command(&format!("m=0;{b}"), (10.0, 20.0), (0, 0));
        assert_eq!(o.reply.as_deref(), Some("\x1b_Gi=3;OK\x1b\\"));
        assert_eq!(g.images[&3].w, 4);
    }
}
