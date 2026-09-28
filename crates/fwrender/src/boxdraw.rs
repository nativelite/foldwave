//! Box drawing, block elements, braille and powerline symbols, drawn in code
//! rather than taken from a font (founder, 2026-09-28).
//!
//! A font draws these per glyph, so lines rarely meet exactly at cell edges
//! and differ from font to font: a TUI's frames show gaps and seams. Here each
//! is drawn to fill its cell exactly, at any size and with any font: lines as
//! whole-pixel strokes centred on the cell, so neighbours always join; blocks
//! as exact fractions of the cell; shades as uniform coverage; braille as a
//! 2x4 grid of dots. Curves and slopes (arcs, diagonals, powerline arrows) go
//! through [`glyphs::Canvas`], anti-aliased.
//!
//! Covered: U+2500..U+257F (box drawing, from a table generated from the
//! Unicode names, `tools/gen_boxdraw.py`), U+2580..U+259F (block elements and
//! shades), U+2800..U+28FF (braille), U+E0B0..U+E0B3 (powerline arrows).

use crate::boxdraw_table::{Kind, BOX};
use glyphs::Canvas;

/// Draw `ch` into `out`, `w x h` coverage bytes, row-major, if it is one of
/// the characters drawn here; returns whether it was.
pub fn draw(ch: char, w: usize, h: usize, out: &mut [u8]) -> bool {
    let c = ch as u32;
    let ours = matches!(c, 0x2500..=0x259F | 0x2800..=0x28FF | 0xE0B0..=0xE0B3);
    if !ours || w == 0 || h == 0 || out.len() < w * h {
        return false;
    }
    let out = &mut out[..w * h];
    out.fill(0);
    let mut g = Grid { w, h, out };
    match c {
        0x2500..=0x257F => {
            let (arms, dashes, kind) = BOX[(c - 0x2500) as usize];
            match kind {
                Kind::Lines if dashes > 0 => g.dashed(arms, dashes),
                Kind::Lines => g.lines(arms),
                Kind::Arc => g.arc(arms),
                Kind::Diagonal => g.diagonal(c),
            }
        }
        0x2580..=0x259F => g.block(c),
        0x2800..=0x28FF => g.braille(c - 0x2800),
        _ => g.powerline(c),
    }
    true
}

/// A cell's coverage and its size.
struct Grid<'a> {
    w: usize,
    h: usize,
    out: &'a mut [u8],
}

/// `len * num / den`, rounded.
fn part(len: usize, num: usize, den: usize) -> usize {
    (len * num + den / 2) / den
}

/// Light stroke width for a cell `w` wide: an eighth of it, at least 1 px.
fn light(w: usize) -> usize {
    ((w + 4) / 8).max(1)
}

/// Where a line of `weight` (1 light, 2 heavy, 3 double) crosses an axis
/// `len` long, centred: one span, or a double's two.
fn bands(weight: u8, len: usize, t: usize) -> Vec<(usize, usize)> {
    let span = |width: usize| {
        let a = len.saturating_sub(width) / 2;
        (a, (a + width).min(len))
    };
    match weight {
        1 => vec![span(t)],
        2 => vec![span(2 * t)],
        3 => {
            let (a, _) = span(3 * t);
            vec![(a, a + t), (a + 2 * t, (a + 3 * t).min(len))]
        }
        _ => Vec::new(),
    }
}

impl Grid<'_> {
    fn fill(&mut self, x0: usize, y0: usize, x1: usize, y1: usize, v: u8) {
        let (x1, y1) = (x1.min(self.w), y1.min(self.h));
        for y in y0..y1 {
            for x in x0..x1 {
                let p = &mut self.out[y * self.w + x];
                *p = (*p).max(v);
            }
        }
    }

    /// Fill a span along one axis and across the other: horizontal arms run
    /// along x, vertical ones along y.
    fn span(&mut self, horizontal: bool, along: (usize, usize), across: (usize, usize)) {
        if along.0 >= along.1 || across.0 >= across.1 {
            return;
        }
        if horizontal {
            self.fill(along.0, across.0, along.1, across.1, 255);
        } else {
            self.fill(across.0, along.0, across.1, along.1, 255);
        }
    }

    /// Solid lines: each arm from its edge to the middle, joined across the
    /// perpendicular strokes; double lines meet at their corners.
    fn lines(&mut self, arms: [u8; 4]) {
        let t = light(self.w);
        let [up, right, down, left] = arms;
        // (weight, at the start of the axis?, perpendicular before and after
        // (the arms on the low and high side across), the opposite arm).
        let horiz = [(left, true), (right, false)];
        let vert = [(up, true), (down, false)];
        for (weight, start) in horiz {
            let opposite = if start { right } else { left };
            self.arm(true, weight, start, up, down, opposite, t);
        }
        for (weight, start) in vert {
            let opposite = if start { down } else { up };
            self.arm(false, weight, start, left, right, opposite, t);
        }
    }

    /// One arm. Along its axis it runs from its edge toward the middle;
    /// `before` and `after` are the perpendicular arms on the low and high
    /// side across it, `opposite` the arm continuing it.
    #[allow(clippy::too_many_arguments)]
    fn arm(
        &mut self,
        horizontal: bool,
        weight: u8,
        start: bool,
        before: u8,
        after: u8,
        opposite: u8,
        t: usize,
    ) {
        if weight == 0 {
            return;
        }
        let (len, across_len) = if horizontal {
            (self.w, self.h)
        } else {
            (self.h, self.w)
        };
        // Work as if the arm starts at 0: mirror an arm at the far edge.
        let flip = |(a, b): (usize, usize)| if start { (a, b) } else { (len - b, len - a) };
        // The perpendicular arms' strokes along this axis, nearest first.
        let perp = |weight: u8| -> Vec<(usize, usize)> {
            let mut v: Vec<_> = bands(weight, len, t).into_iter().map(flip).collect();
            v.sort_unstable();
            v
        };
        let middle = if start { len / 2 } else { len - len / 2 };
        let across = bands(weight, across_len, t);
        if weight == 3 {
            // Each line of a double: toward the middle until it meets what
            // is on its side, else straight on, else round the corner.
            let reach = |same: u8, other: u8| -> usize {
                let (s, o) = (perp(same), perp(other));
                match (s.first(), s.last()) {
                    (Some(near), _) if same == 3 => near.1,
                    (Some(_), Some(far)) => far.1,
                    _ if opposite != 0 => middle,
                    _ => o.last().map_or(middle, |far| far.1),
                }
            };
            let ends = [reach(before, after), reach(after, before)];
            for (band, end) in across.into_iter().zip(ends) {
                self.span(horizontal, flip((0, end)), band);
            }
            return;
        }
        let strokes: Vec<(usize, usize)> = perp(before).into_iter().chain(perp(after)).collect();
        let end = if strokes.is_empty() {
            middle
        } else if [before, after].iter().all(|&p| p == 0 || p == 3) && opposite == 0 {
            // Only doubles across and nothing beyond: stop at the near line.
            strokes.iter().map(|s| s.1).min().unwrap_or(middle)
        } else {
            // Cover the strokes across, so the joint is solid.
            strokes.iter().map(|s| s.1).max().unwrap_or(middle)
        };
        self.span(horizontal, flip((0, end)), across[0]);
    }

    /// A dashed line (U+2504..250B, 254C..254F): `n` dashes across the cell.
    fn dashed(&mut self, arms: [u8; 4], n: u8) {
        let t = light(self.w);
        let horizontal = arms[1] != 0;
        let weight = arms.iter().copied().max().unwrap_or(1);
        let (len, across_len) = if horizontal {
            (self.w, self.h)
        } else {
            (self.h, self.w)
        };
        let across = bands(weight, across_len, t)[0];
        let n = n as usize;
        for i in 0..n {
            let (a, b) = (part(len, i, n), part(len, i + 1, n));
            let gap = ((b - a) / 4).max(1).min((b - a).saturating_sub(1));
            self.span(horizontal, (a + gap / 2, b - (gap - gap / 2)), across);
        }
    }

    /// Rounded corners (U+256D..2570): a quarter curve from the middle of one
    /// edge to the middle of another, light, anti-aliased.
    fn arc(&mut self, arms: [u8; 4]) {
        let t = light(self.w) as f32;
        let (w, h) = (self.w as f32, self.h as f32);
        let bx = bands(1, self.w, light(self.w))[0];
        let by = bands(1, self.h, light(self.w))[0];
        let (cx, cy) = ((bx.0 + bx.1) as f32 / 2.0, (by.0 + by.1) as f32 / 2.0);
        let [up, right, _down, _left] = arms;
        let hx = if right != 0 { w } else { 0.0 };
        let vy = if up != 0 { 0.0 } else { h };
        let (sx, sy) = ((hx - cx).signum(), (vy - cy).signum());
        let o = t / 2.0;
        let mut c = Canvas::new(self.w, self.h);
        let outer = [
            (hx, cy - sy * o),
            (cx - sx * o, cy - sy * o),
            (cx - sx * o, vy),
        ];
        let inner = [
            (cx + sx * o, vy),
            (cx + sx * o, cy + sy * o),
            (hx, cy + sy * o),
        ];
        c.quad(outer[0], outer[1], outer[2]);
        c.line(outer[2].0, outer[2].1, inner[0].0, inner[0].1);
        c.quad(inner[0], inner[1], inner[2]);
        c.line(inner[2].0, inner[2].1, outer[0].0, outer[0].1);
        self.canvas(&c);
    }

    /// Diagonals (U+2571..2573): corner to corner, light, anti-aliased.
    fn diagonal(&mut self, c: u32) {
        let (w, h) = (self.w as f32, self.h as f32);
        // Half the stroke, measured along x, so it is `light` thick across.
        let hw = light(self.w) as f32 / 2.0 * (w * w + h * h).sqrt() / h;
        let mut canvas = Canvas::new(self.w, self.h);
        let mut quad = |pts: [(f32, f32); 4]| {
            for i in 0..4 {
                let (a, b) = (pts[i], pts[(i + 1) % 4]);
                canvas.line(a.0, a.1, b.0, b.1);
            }
        };
        // Upper right to lower left, and upper left to lower right.
        let rising = [(w - hw, 0.0), (w + hw, 0.0), (hw, h), (-hw, h)];
        let falling = [(-hw, 0.0), (hw, 0.0), (w + hw, h), (w - hw, h)];
        if c == 0x2571 || c == 0x2573 {
            quad(rising);
        }
        if c == 0x2572 || c == 0x2573 {
            quad(falling);
        }
        self.canvas(&canvas);
    }

    /// Block elements and shades (U+2580..259F).
    fn block(&mut self, c: u32) {
        let (w, h) = (self.w, self.h);
        let (mx, my) = (part(w, 1, 2), part(h, 1, 2));
        match c {
            0x2580 => self.fill(0, 0, w, my, 255),
            0x2581..=0x2588 => {
                let k = (c - 0x2580) as usize; // eighths from the bottom
                self.fill(0, h - part(h, k, 8), w, h, 255);
            }
            0x2589..=0x258F => {
                let k = (0x2590 - c) as usize; // eighths from the left
                self.fill(0, 0, part(w, k, 8), h, 255);
            }
            0x2590 => self.fill(mx, 0, w, h, 255),
            0x2591 => self.fill(0, 0, w, h, 64),
            0x2592 => self.fill(0, 0, w, h, 128),
            0x2593 => self.fill(0, 0, w, h, 191),
            0x2594 => self.fill(0, 0, w, part(h, 1, 8), 255),
            0x2595 => self.fill(w - part(w, 1, 8), 0, w, h, 255),
            _ => {
                // Quadrants: bit 0 upper left, 1 upper right, 2 lower left,
                // 3 lower right.
                let q: u8 = match c {
                    0x2596 => 0b0100,
                    0x2597 => 0b1000,
                    0x2598 => 0b0001,
                    0x2599 => 0b1101,
                    0x259A => 0b1001,
                    0x259B => 0b0111,
                    0x259C => 0b1011,
                    0x259D => 0b0010,
                    0x259E => 0b0110,
                    _ => 0b1110, // U+259F
                };
                let quads = [
                    (0, 0, mx, my),
                    (mx, 0, w, my),
                    (0, my, mx, h),
                    (mx, my, w, h),
                ];
                for (i, &(x0, y0, x1, y1)) in quads.iter().enumerate() {
                    if q & (1 << i) != 0 {
                        self.fill(x0, y0, x1, y1, 255);
                    }
                }
            }
        }
    }

    /// Braille (U+2800..28FF): dots 1-8 as bits 0-7, in a 2 x 4 grid (dots
    /// 1, 2, 3, 7 down the left, 4, 5, 6, 8 down the right).
    fn braille(&mut self, bits: u32) {
        let at = [
            (0, 0),
            (0, 1),
            (0, 2),
            (1, 0),
            (1, 1),
            (1, 2),
            (0, 3),
            (1, 3),
        ];
        // About a quarter of the cell wide, rounded; never so big that
        // neighbouring dots touch.
        let side = ((self.w + 2) / 4)
            .min((self.w / 2).saturating_sub(1))
            .min((self.h / 4).saturating_sub(1))
            .max(1);
        for (bit, &(col, row)) in at.iter().enumerate() {
            if bits & (1 << bit) == 0 {
                continue;
            }
            let cx = part(self.w, 2 * col + 1, 4);
            let cy = part(self.h, 2 * row + 1, 8);
            let (x0, y0) = (cx.saturating_sub(side / 2), cy.saturating_sub(side / 2));
            self.fill(x0, y0, x0 + side, y0 + side, 255);
        }
    }

    /// Powerline arrows: solid (U+E0B0 right, E0B2 left) and thin (E0B1,
    /// E0B3), the full cell height.
    fn powerline(&mut self, c: u32) {
        let (w, h) = (self.w as f32, self.h as f32);
        let right = c == 0xE0B0 || c == 0xE0B1;
        let x = |v: f32| if right { v } else { w - v };
        let mut canvas = Canvas::new(self.w, self.h);
        let mut poly = |pts: &[(f32, f32)]| {
            for i in 0..pts.len() {
                let (a, b) = (pts[i], pts[(i + 1) % pts.len()]);
                canvas.line(x(a.0), a.1, x(b.0), b.1);
            }
        };
        if c == 0xE0B0 || c == 0xE0B2 {
            poly(&[(0.0, 0.0), (w, h / 2.0), (0.0, h)]);
        } else {
            let t = light(self.w) as f32 * 1.5;
            let dy = t * (h / 2.0) / w;
            poly(&[
                (0.0, 0.0),
                (w, h / 2.0),
                (0.0, h),
                (0.0, h - dy),
                (w - t, h / 2.0),
                (0.0, dy),
            ]);
        }
        self.canvas(&canvas);
    }

    fn canvas(&mut self, c: &Canvas) {
        c.to_bytes(self.out);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `ch` drawn in a `w x h` cell, as rows of '#' (covered) and '.'.
    fn cell(ch: char, w: usize, h: usize) -> Vec<String> {
        let mut out = vec![0u8; w * h];
        assert!(draw(ch, w, h, &mut out), "{ch} is drawn here");
        out.chunks(w)
            .map(|r| {
                r.iter()
                    .map(|&v| if v >= 128 { '#' } else { '.' })
                    .collect()
            })
            .collect()
    }

    #[test]
    fn lines_run_edge_to_edge_through_the_middle() {
        // 8 x 10: light is 1 px, at x 3 and y 4.
        let h = cell('─', 8, 10);
        assert!(h
            .iter()
            .enumerate()
            .all(|(y, r)| (y == 4) == r.contains('#')));
        assert_eq!(h[4], "########", "edge to edge, so neighbours join");
        let v = cell('│', 8, 10);
        assert!(v.iter().all(|r| r == "...#...."));
        let cross = cell('┼', 8, 10);
        assert_eq!(cross[4], "########");
        assert!(cross.iter().all(|r| r.as_bytes()[3] == b'#'));
        // A corner covers its joint: └ runs up from and right of the middle.
        let corner = cell('└', 8, 10);
        assert_eq!(corner[4], "...#####");
        assert_eq!(corner[0], "...#....");
        assert_eq!(corner[9], "........");
    }

    #[test]
    fn heavy_lines_are_thicker_and_mixed_weights_meet() {
        let heavy = cell('━', 8, 10);
        assert_eq!(heavy.iter().filter(|r| r.contains('#')).count(), 2);
        // ┝ vertical light and right heavy: the heavy arm reaches the stem.
        let t = cell('┝', 8, 10);
        assert!(t.iter().all(|r| r.as_bytes()[3] == b'#'));
        assert!(t[4].ends_with("#####") && t[5].ends_with("#####"));
    }

    #[test]
    fn double_lines_meet_at_their_corners() {
        // 16 x 20: light 2 px; a double is lines at 5..7 and 9..11 (x), and
        // 7..9 and 11..13 (y).
        let d = cell('═', 16, 20);
        let rows: Vec<usize> = (0..20).filter(|&y| d[y].contains('#')).collect();
        assert_eq!(rows, [7, 8, 11, 12]);
        let c = cell('╔', 16, 20);
        assert!(
            c[7].starts_with(".....#"),
            "the outer line from x 5: {}",
            c[7]
        );
        assert_eq!(&c[7][5..], "###########");
        // Row 11: the outer vertical line (x 5..7) passes; the inner horizontal
        // line starts at x 9.
        assert_eq!(&c[11][..9], ".....##..", "the inner line starts at x 9");
        assert_eq!(&c[11][9..], "#######");
        assert_eq!(c[19].as_bytes()[5], b'#', "outer vertical to the bottom");
        assert_eq!(
            c[9].as_bytes()[7],
            b'.',
            "the gap between the lines stays open"
        );
        // ╦: the outer line runs across; the inner one stops at each
        // vertical's outer edge, so each meets its own vertical.
        let t = cell('╦', 16, 20);
        assert_eq!(t[7], "################");
        assert_eq!(t[11], "#######..#######");
        assert_eq!(t[13], ".....##..##.....");
        // ╠: the left vertical runs through; the right side is two corners.
        let l = cell('╠', 16, 20);
        assert_eq!(l[0], ".....##..##.....");
        assert_eq!(l[7], ".....##..#######");
        assert_eq!(l[9], ".....##.........");
        assert_eq!(l[11], ".....##..#######");
        // ╬: four inner corners, the middle open.
        let x = cell('╬', 16, 20);
        assert_eq!(x[9].as_bytes()[7], b'.');
        assert_eq!(x[0].as_bytes()[5], b'#');
        assert_eq!(x[7].as_bytes()[0], b'#');
    }

    #[test]
    fn blocks_are_exact_fractions_and_shades_uniform() {
        assert_eq!(cell('▀', 4, 8)[..4], ["####"; 4]);
        assert_eq!(cell('▀', 4, 8)[4..], ["...."; 4]);
        let low = cell('▁', 4, 8);
        assert_eq!(low.iter().filter(|r| r.contains('#')).count(), 1);
        assert_eq!(low[7], "####");
        assert!(cell('▌', 4, 8).iter().all(|r| r == "##.."));
        assert!(cell('▐', 4, 8).iter().all(|r| r == "..##"));
        assert!(cell('█', 4, 8).iter().all(|r| r == "####"));
        let q = cell('▚', 4, 8);
        assert_eq!((q[0].as_str(), q[7].as_str()), ("##..", "..##"));
        let mut out = vec![0u8; 4 * 8];
        draw('▒', 4, 8, &mut out);
        assert!(out.iter().all(|&v| v == 128));
    }

    #[test]
    fn braille_dots_sit_in_their_grid() {
        let one = cell('\u{2801}', 8, 16); // dot 1: top left
        let lit: Vec<(usize, usize)> = (0..16)
            .flat_map(|y| (0..8).map(move |x| (x, y)))
            .filter(|&(x, y)| one[y].as_bytes()[x] == b'#')
            .collect();
        assert!(lit.iter().all(|&(x, y)| x < 4 && y < 4), "{lit:?}");
        assert!(!lit.is_empty());
        let all = cell('\u{28FF}', 8, 16);
        let count = all.iter().map(|r| r.matches('#').count()).sum::<usize>();
        assert_eq!(count, 8 * 2 * 2, "eight 2 x 2 dots");
        // An 11 x 23 cell (JetBrains Mono at 18 px): 3 x 3 dots, apart.
        let big = cell('⣿', 11, 23);
        let count = big.iter().map(|r| r.matches('#').count()).sum::<usize>();
        assert_eq!(count, 8 * 3 * 3, "eight 3 x 3 dots");
        assert!(big
            .iter()
            .all(|r| !r.contains("#.#") && r.matches('#').count() <= 6));
    }

    #[test]
    fn curves_and_arrows_are_drawn_and_other_characters_are_not() {
        let arc = cell('╭', 16, 20);
        assert_eq!(arc[9].as_bytes()[15], b'#', "meets the right edge's line");
        assert_eq!(arc[19].as_bytes()[7], b'#', "meets the bottom edge's line");
        assert_eq!(arc[0].as_bytes()[0], b'.', "not the far corner");
        let arrow = cell('\u{E0B0}', 8, 16);
        assert_eq!(arrow[8].as_bytes()[0], b'#');
        assert_eq!(arrow[0].as_bytes()[7], b'.');
        let diag = cell('╱', 8, 16);
        assert_eq!(diag[0].as_bytes()[7], b'#');
        assert_eq!(diag[15].as_bytes()[0], b'#');
        let mut out = vec![0u8; 16];
        assert!(!draw('a', 4, 4, &mut out));
        assert!(!draw('─', 0, 4, &mut out));
    }
}
