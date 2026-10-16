//! Scrollback search: plain text (not a regular expression), newest match
//! first, Enter for older ones, Shift+Enter for newer ones. Matches are shown
//! as the selection, so copying a found match is just Ctrl+Shift+C.

use alacritty_terminal::event::EventListener;
use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::index::{Boundary, Column, Direction, Line, Point, Side};
use alacritty_terminal::selection::{Selection, SelectionType};
use alacritty_terminal::term::search::{Match, RegexSearch};
use alacritty_terminal::term::Term;

#[derive(Default)]
pub struct Search {
    pub query: String,
    regex: Option<RegexSearch>,
    current: Option<Match>,
    pub found: bool,
}

/// The search box finds text, not regular expressions.
fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if "\\.+*?()|[]{}^$#&-~".contains(c) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

impl Search {
    /// The query changed: search again from the bottom (the newest output).
    pub fn set_query<T: EventListener>(&mut self, t: &mut Term<T>, q: &str) {
        self.query = q.to_string();
        self.regex = if q.is_empty() { None } else { RegexSearch::new(&escape(q)).ok() };
        self.current = None;
        if self.regex.is_some() {
            self.step(t, Direction::Left);
        } else {
            self.found = false;
            t.selection = None;
        }
    }

    /// Left: the next older match; Right: the next newer one.
    pub fn step<T: EventListener>(&mut self, t: &mut Term<T>, dir: Direction) {
        let Some(re) = self.regex.as_mut() else { return };
        let cols = t.grid().columns();
        let bottom = Point::new(Line(t.screen_lines() as i32 - 1), Column(cols.saturating_sub(1)));
        let origin = match (&self.current, dir) {
            (Some(m), Direction::Left) => m.start().sub(&*t, Boundary::None, 1),
            (Some(m), Direction::Right) => m.end().add(&*t, Boundary::None, 1),
            (None, _) => bottom,
        };
        let side = if dir == Direction::Left { Side::Right } else { Side::Left };
        match t.search_next(re, origin, dir, side, None) {
            Some(m) => {
                let mut sel = Selection::new(SelectionType::Simple, *m.start(), Side::Left);
                sel.update(*m.end(), Side::Right);
                t.selection = Some(sel);
                t.scroll_to_point(*m.start());
                self.current = Some(m);
                self.found = true;
            }
            None if self.current.is_none() => {
                t.selection = None;
                self.found = false;
            }
            None => {}
        }
    }

    pub fn close<T: EventListener>(&mut self, t: &mut Term<T>) {
        t.selection = None;
        t.scroll_display(Scroll::Bottom);
    }

    /// The line shown in the search bar.
    pub fn bar(&self) -> String {
        let state = if self.query.is_empty() { "type to search" } else if self.found { "Enter older · Shift+Enter newer" } else { "no match" };
        format!("Search: {}▏  {state}  · Esc", self.query)
    }

    /// The current match's start, as (absolute line from the top of history, column).
    pub fn current_line<T>(&self, t: &Term<T>) -> Option<usize> {
        self.current.as_ref().map(|m| (m.start().line.0 + t.grid().history_size() as i32) as usize)
    }
}
