//! What a copy puts on the clipboard: the selection, tidied (`[copy]` in zash.toml). Programs
//! like Claude Code, and multiplexers like zeit, wrap long lines themselves: the terminal sees
//! real line breaks, so a long command copied from them pasted as several broken lines. `unwrap`
//! puts those lines back together: a line was wrapped when the next line's first word would not
//! have fit on it (how a wrap decides), so lines ended on purpose stay apart.

#[derive(Clone, Debug, PartialEq)]
pub struct Settings {
    /// join lines a program wrapped ("smart"), or copy rows as they are ("off")
    pub unwrap: bool,
    /// drop spaces at line ends
    pub trim: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Settings { unwrap: true, trim: true }
    }
}

/// A line that starts something of its own: a list item, a quote, a fence, a table or box row.
fn starts_own(line: &str) -> bool {
    let t = line.trim_start();
    let num = t.split_once(". ").is_some_and(|(n, _)| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()));
    num || ["- ", "* ", "+ ", "• ", "● ", "⎿ ", "> ", "```", "│", "|", "┃", "#", "$ "].iter().any(|p| t.starts_with(p))
}

/// The selection as it goes to the clipboard.
pub fn tidy(text: &str, s: &Settings) -> String {
    let lines: Vec<&str> = text.split('\n').map(|l| if s.trim { l.trim_end() } else { l }).collect();
    if !s.unwrap || lines.len() < 2 {
        return lines.join("\n");
    }
    // the wrap width: the longest line (a wrapped paragraph fills up to it)
    let width = lines.iter().map(|l| l.chars().count()).max().unwrap_or(0);
    let mut out = String::new();
    for (i, l) in lines.iter().enumerate() {
        if i == 0 {
            out.push_str(l);
            continue;
        }
        let prev = lines[i - 1];
        let next_word = l.trim_start().split(' ').next().unwrap_or("");
        // a wrap needs a wide block and a line that fills most of it: short lines are ended on purpose
        let wrapped = width >= 24
            && prev.chars().count() * 10 >= width * 6
            && !prev.trim().is_empty()
            && !l.trim().is_empty()
            && !starts_own(l)
            // the next word would not have fit after the previous line
            && prev.chars().count() + 1 + next_word.chars().count() > width;
        if wrapped {
            out.push(' ');
            out.push_str(l.trim_start());
        } else {
            out.push('\n');
            out.push_str(l);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_wrapped_command_comes_back_whole() {
        // as Claude Code wraps it at 40 columns, indented
        let copied = "  sudo cp /mnt/storage/dev/projects/knurl\n  /packaging/70-knurl-pen.rules /etc/udev\n  /rules.d/";
        // a path wrapped inside a word cannot be told from two words: it joins with a space,
        // which is what a wrap in a word looks like; whole words are the common case:
        let words = "  echo one two three four five six seven\n  eight nine ten";
        assert_eq!(tidy(words, &Settings::default()), "  echo one two three four five six seven eight nine ten");
        assert!(tidy(copied, &Settings::default()).lines().count() == 1);
    }

    #[test]
    fn lines_ended_on_purpose_stay_apart() {
        let text = "Two things:\n- the first one\n- the second one\nshort\nalso short";
        assert_eq!(tidy(text, &Settings::default()), text);
        let code = "```sh\nls -la\ncd /tmp\n```";
        assert_eq!(tidy(code, &Settings::default()), code);
    }

    #[test]
    fn spaces_at_line_ends_go_and_off_means_off() {
        assert_eq!(tidy("a   \nb  ", &Settings::default()), "a\nb");
        let off = Settings { unwrap: false, trim: false };
        let text = "  echo one two three four five six seven\n  eight";
        assert_eq!(tidy(text, &off), text);
    }
}
