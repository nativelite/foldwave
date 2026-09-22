//! The CPU renderer: software rendering into a `0xAARRGGBB` framebuffer.
//!
//! It is the fallback when no GPU backend is available and the reference the
//! GPU backends are tested against. It redraws only damaged rows.

use crate::atlas::Atlas;
use crate::frame::{Frame, STRIKE, UNDERLINE};

/// One channel of the pixel rule: `(bg * (255 - a) + fg * a + 127) / 255`,
/// applied to R, G and B; alpha is 255.
#[inline]
pub fn blend(fg: u32, bg: u32, a: u8) -> u32 {
    if a == 0 {
        return bg | 0xFF00_0000;
    }
    if a == 255 {
        return fg | 0xFF00_0000;
    }
    let a = a as u32;
    let mut out = 0xFF00_0000;
    for shift in [0, 8, 16] {
        let (f, b) = ((fg >> shift) & 0xFF, (bg >> shift) & 0xFF);
        out |= ((b * (255 - a) + f * a + 127) / 255) << shift;
    }
    out
}

/// The software renderer.
#[derive(Debug, Clone)]
pub struct CpuRenderer {
    width: u32,
    height: u32,
    pixels: Vec<u32>,
    /// Background the margins were last filled with; a change repaints them.
    margin: Option<u32>,
}

impl CpuRenderer {
    /// A `width x height` framebuffer.
    pub fn new(width: u32, height: u32) -> CpuRenderer {
        CpuRenderer {
            width,
            height,
            pixels: vec![0; (width * height) as usize],
            margin: None,
        }
    }

    /// Width in pixels.
    pub fn width(&self) -> u32 {
        self.width
    }

    /// Height in pixels.
    pub fn height(&self) -> u32 {
        self.height
    }

    /// The framebuffer, row-major, `width` pixels per row.
    pub fn pixels(&self) -> &[u32] {
        &self.pixels
    }

    /// Resize; the next [`CpuRenderer::draw`] must see every row damaged.
    pub fn resize(&mut self, width: u32, height: u32) {
        self.width = width;
        self.height = height;
        self.pixels = vec![0; (width * height) as usize];
        self.margin = None;
    }

    /// Draw the frame's damaged rows. Returns the rows drawn; the caller
    /// clears the damage once every consumer has drawn.
    pub fn draw(&mut self, frame: &Frame, atlas: &Atlas) -> usize {
        let m = atlas.metrics();
        let (cw, ch) = (m.cell_w, m.cell_h);
        let grid_w = (frame.cols() as u32 * cw).min(self.width);
        let grid_h = (frame.rows() as u32 * ch).min(self.height);
        if self.margin != Some(frame.background()) {
            let bg = frame.background();
            for y in 0..self.height {
                let row = &mut self.pixels[(y * self.width) as usize..][..self.width as usize];
                let from = if y < grid_h { grid_w } else { 0 };
                row[from as usize..].iter_mut().for_each(|p| *p = bg);
            }
            self.margin = Some(bg);
        }
        let apix = atlas.pixels();
        let aw = atlas.width() as usize;
        let mut drawn = 0;
        for (r, _) in frame.damage().iter().enumerate().filter(|(_, &d)| d) {
            let top = r as u32 * ch;
            if top >= self.height {
                break;
            }
            drawn += 1;
            for (c, cell) in frame.row(r).iter().enumerate() {
                let left = c as u32 * cw;
                if left >= self.width {
                    break;
                }
                let (ox, oy) = atlas.slot_origin(cell.glyph);
                for y in 0..ch.min(self.height - top) {
                    let line = (cell.flags & UNDERLINE != 0 && ch >= 3 && y == ch - 2)
                        || (cell.flags & STRIKE != 0 && y == ch / 2);
                    let src = &apix[(oy + y) as usize * aw + ox as usize..][..cw as usize];
                    let dst = &mut self.pixels[((top + y) * self.width + left) as usize..]
                        [..cw.min(self.width - left) as usize];
                    for (x, p) in dst.iter_mut().enumerate() {
                        let a = if line { 255 } else { src[x] };
                        *p = blend(cell.fg, cell.bg, a);
                    }
                }
            }
        }
        drawn
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::atlas::{Metrics, TestRasterizer};
    use crate::frame::Palette;
    use ansi::{Screen, Style};

    const M: Metrics = Metrics {
        cell_w: 8,
        cell_h: 16,
        baseline: 12,
    };

    #[test]
    fn blend_hits_both_ends_and_rounds() {
        assert_eq!(blend(0xFFFFFFFF, 0xFF000000, 0), 0xFF000000);
        assert_eq!(blend(0xFFFFFFFF, 0xFF000000, 255), 0xFFFFFFFF);
        assert_eq!(blend(0xFFFFFFFF, 0xFF000000, 128), 0xFF808080);
        assert_eq!(blend(0xFF0000FF, 0xFFFF0000, 51), 0xFFCC0033);
    }

    #[test]
    fn a_frame_draws_per_the_pixel_rule_and_only_damaged_rows() {
        let p = Palette::default();
        let mut a = Atlas::new(M, 256, 256);
        let mut r = TestRasterizer { metrics: M };
        let mut s = Screen::new(2, 3);
        s.write_str(
            0,
            0,
            "ab",
            Style {
                underline: true,
                ..Style::default()
            },
        );
        let mut f = Frame::new();
        f.update(&s, false, &p, &mut a, &mut r);
        // One pixel of margin on the right and at the bottom.
        let mut cpu = CpuRenderer::new(3 * 8 + 1, 2 * 16 + 1);
        assert_eq!(cpu.draw(&f, &a), 2);
        f.clear_damage();

        let cell = f.row(0)[1];
        let (ox, oy) = a.slot_origin(cell.glyph);
        for y in 0..16u32 {
            for x in 0..8u32 {
                let cov = a.pixels()[((oy + y) * 256 + ox + x) as usize];
                let cov = if y == 14 { 255 } else { cov };
                let got = cpu.pixels()[(y * cpu.width() + 8 + x) as usize];
                assert_eq!(got, blend(cell.fg, cell.bg, cov), "({x}, {y})");
            }
        }
        assert_eq!(
            cpu.pixels()[(cpu.width() - 1) as usize],
            p.bg,
            "right margin"
        );
        assert_eq!(*cpu.pixels().last().unwrap(), p.bg, "bottom margin");

        assert_eq!(cpu.draw(&f, &a), 0, "nothing damaged, nothing drawn");
        s.write_str(1, 0, "z", Style::default());
        f.update(&s, false, &p, &mut a, &mut r);
        assert_eq!(cpu.draw(&f, &a), 1);
    }
}
