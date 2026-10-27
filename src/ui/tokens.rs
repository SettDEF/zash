//! Design tokens for UI built on the cell grid: the desktop shell's scale (quickshell-ui skill) mapped to
//! cells. A cell is about 9.5 x 19 px at the default font, so a px value becomes whole cells across and
//! down (8 px rounds to 0 rows, 14 px to 1), and only pixel insets the escape can carry stay fractional.

#![allow(dead_code)]

/// Spacing: shell 5–6 px (chips) · 8 px (within a row) · 14 px (between sections).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Gap {
    None,
    /// 5–6 px: 1 col, 0 rows
    Tight,
    /// 8 px: 1 col, 0 rows
    Row,
    /// 14 px: 2 cols, 1 row
    Section,
}

impl Gap {
    /// The gap in cells along a row (across) or a column (down).
    pub fn cells(self, across: bool) -> usize {
        match (self, across) {
            (Gap::None, _) => 0,
            (Gap::Tight | Gap::Row, true) => 1,
            (Gap::Tight | Gap::Row, false) => 0,
            (Gap::Section, true) => 2,
            (Gap::Section, false) => 1,
        }
    }
}

/// Card inner margin (16 px): 2 cols, 1 row; the panel is the card's cells, so the margin is even.
pub const CARD_PAD: [usize; 4] = [2, 1, 2, 1];
/// A page's margin from the sidebar and the top (Dotos: 20 px, 14 px).
pub const PAGE_PAD: [usize; 4] = [3, 1, 3, 1];
/// Sidebar inner margin: 1 col each side, 1 row top and bottom.
pub const SIDEBAR_PAD: [usize; 4] = [1, 1, 1, 1];
/// A list row's icon and label offsets inside the row (Dotos: icon 12 px in, label 40 px in).
pub const ITEM_ICON: usize = 1;
pub const ITEM_LABEL: usize = 4;
/// A section label's indent: between the icon and the label.
pub const SECTION_INDENT: usize = 2;
/// A selected row's pill reaches this far past its cells (cells across; ~3 px).
pub const PILL_INSET: f32 = 0.3;

/// Rounding in px: verysmall 8 · small 12 · normal 17 · large 23 · full.
pub mod radius {
    pub const VERYSMALL: f32 = 8.0;
    pub const SMALL: f32 = 12.0;
    pub const NORMAL: f32 = 17.0;
    pub const LARGE: f32 = 23.0;
}

/// The type scale. Body is the cell font (shell small 15 px); the others relative to it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Type {
    /// smaller 12 px
    Caption,
    /// small 15 px
    Body,
    /// 17 px, semibold
    Title,
    /// 24 px, two rows
    Heading,
    /// 32 px, two rows
    Display,
}

impl Type {
    pub fn scale(self) -> f32 {
        match self {
            Type::Caption => 0.8,
            Type::Body => 1.0,
            Type::Title => 1.15,
            Type::Heading => 1.6,
            Type::Display => 2.0,
        }
    }
    pub fn weight(self) -> u16 {
        match self {
            Type::Caption => 500,
            Type::Body => 400,
            Type::Title => 600,
            Type::Heading | Type::Display => 600,
        }
    }
    pub fn rows(self) -> usize {
        match self {
            Type::Heading | Type::Display => 2,
            _ => 1,
        }
    }
    /// Cells the words take: the UI font at a bigger scale is wider than the cells.
    pub fn width(self, chars: usize) -> usize {
        (chars as f32 * self.scale().max(1.0)).ceil() as usize
    }
}

/// Colour roles as the shell's layers (zash's role names, ui.rs `role`).
pub mod role {
    /// colLayer0: the pane
    pub const LAYER0: &str = "surface";
    /// colLayer1: sidebar, status line
    pub const LAYER1: &str = "surface-container-high";
    /// colLayer2: cards
    pub const LAYER2: &str = "surface-container-highest";
    /// colLayer3: chips, secondary buttons
    pub const LAYER3: &str = "surface-container-highest";
    /// the current line: barely lifted
    pub const HIGHLIGHT: &str = "surface-container";
    /// a step under layer 1: the activity bar
    pub const SUNK: &str = "surface-container";
    pub const ON: &str = "on-surface";
    pub const SUBTEXT: &str = "on-surface-variant";
    pub const PRIMARY: &str = "primary";
    pub const PRIMARY_CONTAINER: &str = "primary-container";
    pub const ON_PRIMARY_CONTAINER: &str = "on-primary-container";
    pub const BORDER: &str = "outline-variant";
    /// lines between parts
    pub const LINE: &str = "outline";
}
