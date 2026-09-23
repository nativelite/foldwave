//! A frame: one [`CellInstance`] per cell, and which rows changed.
//!
//! [`Frame::update`] resolves an [`ansi::Screen`] into instances — colours
//! through the [`Palette`], reverse video, dim, the block cursor, and a glyph
//! slot from the [`Atlas`] — and compares each row with what the frame held.
//! Rows that differ are marked damaged; a backend redraws (CPU) or re-uploads
//! (GPU) only those, and draws nothing at all when no row is damaged.

use crate::atlas::{Atlas, GlyphKey, GlyphPart, Rasterizer, BOLD, ITALIC};
use ansi::{CellWidth, Color, Screen};

/// Cell flag: draw an underline.
pub const UNDERLINE: u32 = 1;
/// Cell flag: draw a strike-through.
pub const STRIKE: u32 = 2;

/// What a backend draws for one cell: 16 bytes, laid out for a GPU instance
/// buffer.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CellInstance {
    /// Atlas slot; 0 is blank.
    pub glyph: u32,
    /// Foreground, `0xAARRGGBB`.
    pub fg: u32,
    /// Background, `0xAARRGGBB`.
    pub bg: u32,
    /// [`UNDERLINE`] and [`STRIKE`] bits.
    pub flags: u32,
}

/// Colours: the terminal's default foreground and background and the 256
/// indexed colours (xterm's defaults).
#[derive(Debug, Clone)]
pub struct Palette {
    /// Default foreground.
    pub fg: u32,
    /// Default background, and the colour outside the grid.
    pub bg: u32,
    /// Indexed colours 0–255.
    pub indexed: [u32; 256],
}

impl Default for Palette {
    fn default() -> Self {
        const BASE: [u32; 16] = [
            0x000000, 0xCD0000, 0x00CD00, 0xCDCD00, 0x0000EE, 0xCD00CD, 0x00CDCD, 0xE5E5E5,
            0x7F7F7F, 0xFF0000, 0x00FF00, 0xFFFF00, 0x5C5CFF, 0xFF00FF, 0x00FFFF, 0xFFFFFF,
        ];
        const LEVEL: [u32; 6] = [0, 95, 135, 175, 215, 255];
        let mut indexed = [0u32; 256];
        for (i, c) in BASE.iter().enumerate() {
            indexed[i] = 0xFF00_0000 | c;
        }
        for i in 0..216 {
            let (r, g, b) = (LEVEL[i / 36], LEVEL[(i / 6) % 6], LEVEL[i % 6]);
            indexed[16 + i] = 0xFF00_0000 | r << 16 | g << 8 | b;
        }
        for i in 0..24 {
            let v = 8 + 10 * i as u32;
            indexed[232 + i] = 0xFF00_0000 | v << 16 | v << 8 | v;
        }
        Palette {
            fg: 0xFFCC_CCCC,
            bg: 0xFF0C_0C0C,
            indexed,
        }
    }
}

impl Palette {
    fn resolve(&self, c: Color, default: u32, bright: bool) -> u32 {
        match c {
            Color::Default => default,
            Color::Indexed(n) if bright && n < 8 => self.indexed[n as usize + 8],
            Color::Indexed(n) => self.indexed[n as usize],
            Color::Rgb(r, g, b) => 0xFF00_0000 | (r as u32) << 16 | (g as u32) << 8 | b as u32,
        }
    }
}

/// Average of two colours, channel by channel (used for dim text).
fn halfway(a: u32, b: u32) -> u32 {
    let mut out = 0xFF00_0000;
    for shift in [0, 8, 16] {
        let (x, y) = ((a >> shift) & 0xFF, (b >> shift) & 0xFF);
        out |= ((x + y) / 2) << shift;
    }
    out
}

/// The frame.
#[derive(Debug, Clone)]
pub struct Frame {
    rows: usize,
    cols: usize,
    cells: Vec<CellInstance>,
    damage: Vec<bool>,
    background: u32,
    scratch: Vec<CellInstance>,
}

impl Frame {
    /// An empty frame; the first [`Frame::update`] damages every row.
    pub fn new() -> Frame {
        Frame {
            rows: 0,
            cols: 0,
            cells: Vec::new(),
            damage: Vec::new(),
            background: 0,
            scratch: Vec::new(),
        }
    }

    /// Rows.
    pub fn rows(&self) -> usize {
        self.rows
    }

    /// Columns.
    pub fn cols(&self) -> usize {
        self.cols
    }

    /// Every instance, row-major.
    pub fn cells(&self) -> &[CellInstance] {
        &self.cells
    }

    /// One row's instances.
    pub fn row(&self, r: usize) -> &[CellInstance] {
        &self.cells[r * self.cols..(r + 1) * self.cols]
    }

    /// The colour outside the grid.
    pub fn background(&self) -> u32 {
        self.background
    }

    /// Per row: changed since the last [`Frame::clear_damage`].
    pub fn damage(&self) -> &[bool] {
        &self.damage
    }

    /// Rows currently damaged.
    pub fn damaged_rows(&self) -> usize {
        self.damage.iter().filter(|&&d| d).count()
    }

    /// Mark every row drawn.
    pub fn clear_damage(&mut self) {
        self.damage.iter_mut().for_each(|d| *d = false);
    }

    /// Mark every row damaged (after a resize of the target, a lost device).
    pub fn damage_all(&mut self) {
        self.damage.iter_mut().for_each(|d| *d = true);
    }

    /// Bring the frame up to date with `screen`. A block cursor is drawn at
    /// the screen's cursor when `cursor` is true. Returns how many rows became
    /// damaged by this call.
    pub fn update(
        &mut self,
        screen: &Screen,
        cursor: bool,
        palette: &Palette,
        atlas: &mut Atlas,
        raster: &mut dyn Rasterizer,
    ) -> usize {
        let at = cursor.then(|| screen.cursor());
        self.update_with(
            screen.rows(),
            screen.cols(),
            at,
            |r, c| screen.cell(r, c),
            palette,
            atlas,
            raster,
        )
    }

    /// Like [`Frame::update`], with the view scrolled back `back` lines into
    /// `history`'s scrollback (`ansi::Screen::history_cell`): the top `back`
    /// rows show history, newest lowest, and the rest the top of `screen`. The
    /// cursor shows only if it is still in view. `back` is clamped to the
    /// history held.
    #[allow(clippy::too_many_arguments)]
    pub fn update_view(
        &mut self,
        screen: &Screen,
        history: &Screen,
        back: usize,
        cursor: bool,
        palette: &Palette,
        atlas: &mut Atlas,
        raster: &mut dyn Rasterizer,
    ) -> usize {
        let back = back.min(history.history_len());
        let rows = screen.rows();
        let at = screen.cursor();
        let at = (cursor && at.row + back < rows).then(|| ansi::Cursor::new(at.row + back, at.col));
        self.update_with(
            rows,
            screen.cols(),
            at,
            |r, c| {
                if r < back {
                    history.history_cell(back - 1 - r, c)
                } else {
                    screen.cell(r - back, c)
                }
            },
            palette,
            atlas,
            raster,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn update_with(
        &mut self,
        rows: usize,
        cols: usize,
        at: Option<ansi::Cursor>,
        cell_at: impl Fn(usize, usize) -> ansi::Cell,
        palette: &Palette,
        atlas: &mut Atlas,
        raster: &mut dyn Rasterizer,
    ) -> usize {
        let mut newly = 0;
        if rows != self.rows || cols != self.cols || palette.bg != self.background {
            self.rows = rows;
            self.cols = cols;
            self.cells = vec![CellInstance::default(); rows * cols];
            self.damage = vec![true; rows];
            self.background = palette.bg;
            newly = rows;
        }
        for r in 0..rows {
            self.scratch.clear();
            let mut lead = ' ';
            for c in 0..cols {
                let cell = cell_at(r, c);
                let s = cell.style;
                let mut fg = palette.resolve(s.fg, palette.fg, s.bold);
                let mut bg = palette.resolve(s.bg, palette.bg, false);
                if s.reverse {
                    std::mem::swap(&mut fg, &mut bg);
                }
                if s.dim {
                    fg = halfway(fg, bg);
                }
                if at.map_or(false, |a| a.row == r && a.col == c) {
                    std::mem::swap(&mut fg, &mut bg);
                }
                let style = if s.bold { BOLD } else { 0 } | if s.italic { ITALIC } else { 0 };
                let (ch, part) = match cell.width {
                    CellWidth::Single => (cell.ch, GlyphPart::Single),
                    CellWidth::Wide => {
                        lead = cell.ch;
                        (cell.ch, GlyphPart::Left)
                    }
                    CellWidth::Continuation => (lead, GlyphPart::Right),
                };
                let glyph = if ch == ' ' {
                    0
                } else {
                    atlas.slot(GlyphKey { ch, style, part }, raster)
                };
                if cell.width != CellWidth::Wide {
                    lead = ' ';
                }
                let flags =
                    if s.underline { UNDERLINE } else { 0 } | if s.strike { STRIKE } else { 0 };
                self.scratch.push(CellInstance {
                    glyph,
                    fg,
                    bg,
                    flags,
                });
            }
            let row = &mut self.cells[r * cols..(r + 1) * cols];
            if row != &self.scratch[..] {
                row.copy_from_slice(&self.scratch);
                if !self.damage[r] {
                    self.damage[r] = true;
                    newly += 1;
                }
            }
        }
        newly
    }
}

impl Default for Frame {
    fn default() -> Self {
        Frame::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::atlas::{Metrics, TestRasterizer};
    use ansi::{Cell, Cursor, Style};

    const M: Metrics = Metrics {
        cell_w: 8,
        cell_h: 16,
        baseline: 12,
    };

    fn setup() -> (Palette, Atlas, TestRasterizer) {
        (
            Palette::default(),
            Atlas::new(M, 256, 256),
            TestRasterizer { metrics: M },
        )
    }

    #[test]
    fn only_changed_rows_are_damaged() {
        let (p, mut a, mut r) = setup();
        let mut s = Screen::new(4, 10);
        s.write_str(0, 0, "hello", Style::default());
        let mut f = Frame::new();
        assert_eq!(
            f.update(&s, false, &p, &mut a, &mut r),
            4,
            "first frame: all"
        );
        f.clear_damage();
        assert_eq!(
            f.update(&s, false, &p, &mut a, &mut r),
            0,
            "no change, no damage"
        );
        s.write_str(2, 3, "x", Style::default());
        assert_eq!(f.update(&s, false, &p, &mut a, &mut r), 1);
        assert_eq!(f.damage(), [false, false, true, false]);
    }

    #[test]
    fn colours_reverse_bold_and_cursor_resolve() {
        let (p, mut a, mut r) = setup();
        let mut s = Screen::new(1, 4);
        let red_on_blue = Style {
            fg: Color::Indexed(1),
            bg: Color::Rgb(0, 0, 255),
            ..Style::default()
        };
        s.set(0, 0, Cell::new('a', red_on_blue));
        s.set(
            0,
            1,
            Cell::new(
                'b',
                Style {
                    reverse: true,
                    ..red_on_blue
                },
            ),
        );
        s.set(
            0,
            2,
            Cell::new(
                'c',
                Style {
                    bold: true,
                    ..red_on_blue
                },
            ),
        );
        s.set_cursor(Cursor::new(0, 3));
        let mut f = Frame::new();
        f.update(&s, true, &p, &mut a, &mut r);
        let row = f.row(0);
        assert_eq!((row[0].fg, row[0].bg), (0xFFCD0000, 0xFF0000FF));
        assert_eq!((row[1].fg, row[1].bg), (0xFF0000FF, 0xFFCD0000));
        assert_eq!(row[2].fg, 0xFFFF0000, "bold brightens the first eight");
        assert_ne!(row[2].glyph, row[0].glyph);
        assert_eq!((row[3].fg, row[3].bg), (p.bg, p.fg), "block cursor");
        assert_eq!(row[3].glyph, 0, "a space is blank");
    }

    #[test]
    fn a_scrolled_back_view_shows_history_above_the_screen() {
        let (p, mut a, mut r) = setup();
        let mut s = Screen::with_history(3, 4, 10);
        for k in 0..6u8 {
            s.scroll_rows_up(0, 2, 1, Cell::default());
            s.write_str(2, 0, &format!("L{k}"), Style::default());
        }
        s.set_cursor(Cursor::new(2, 2));
        // On screen L3, L4, L5; history newest first L2, L1, L0, blanks.
        let ch = |f: &Frame, a: &Atlas, row: usize| f.row(row)[1].glyph != 0 && a.used() > 0;
        let mut f = Frame::new();
        f.update_view(&s, &s, 0, true, &p, &mut a, &mut r);
        let at_bottom: Vec<u32> = (0..3).map(|row| f.row(row)[1].glyph).collect();
        f.update_view(&s, &s, 2, true, &p, &mut a, &mut r);
        // Scrolled back 2: rows show L1, L2, L3; the cursor (on L5) is out of view.
        assert_eq!(
            f.row(2)[1].glyph,
            at_bottom[0],
            "L3 moved down to the last row"
        );
        assert!(ch(&f, &a, 0));
        assert!(f.cells().iter().all(|c| c.bg == p.bg), "no cursor drawn");
        let mut plain = Frame::new();
        plain.update(&s, true, &p, &mut a, &mut r);
        let mut zero = Frame::new();
        zero.update_view(&s, &s, 0, true, &p, &mut a, &mut r);
        assert_eq!(plain.cells(), zero.cells(), "back 0 is the plain view");
        // Asking for more than is held clamps to the history.
        let mut far = Frame::new();
        far.update_view(&s, &s, 1_000, false, &p, &mut a, &mut r);
        let mut max = Frame::new();
        max.update_view(&s, &s, s.history_len(), false, &p, &mut a, &mut r);
        assert_eq!(far.cells(), max.cells());
    }

    #[test]
    fn a_wide_glyph_uses_its_two_halves() {
        let (p, mut a, mut r) = setup();
        let mut s = Screen::new(1, 4);
        s.set(0, 1, Cell::wide('界', Style::default()));
        s.set(0, 2, Cell::continuation(Style::default()));
        s.set(0, 3, Cell::continuation(Style::default())); // orphan
        let mut f = Frame::new();
        f.update(&s, false, &p, &mut a, &mut r);
        let row = f.row(0);
        assert_eq!(row[2].glyph, row[1].glyph + 1);
        assert_eq!(row[3].glyph, 0, "an orphan half draws blank");
    }
}
