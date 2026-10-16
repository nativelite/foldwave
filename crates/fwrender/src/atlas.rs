//! The glyph atlas: every glyph rasterized once, as 8-bit coverage, in a
//! fixed grid of cell-sized slots.
//!
//! Slot 0 is always blank (all zero), so a space costs nothing. A wide glyph
//! is rasterized once at two cells' width and stored as two slots, its left
//! and right halves, so every cell still maps to exactly one slot.
//!
//! The atlas never evicts in this spike: when it is full, new glyphs draw as
//! blank and are counted in [`Atlas::misses`]. At a 10x20 cell a 2048x2048
//! atlas holds about 20,000 glyphs.

use std::collections::HashMap;
use std::ops::Range;

/// Glyph style bit: bold.
pub const BOLD: u8 = 1;
/// Glyph style bit: italic.
pub const ITALIC: u8 = 2;

/// Cell size in pixels, and where the text baseline sits inside a cell.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Metrics {
    /// Cell width in pixels.
    pub cell_w: u32,
    /// Cell height in pixels.
    pub cell_h: u32,
    /// Baseline, in pixels from the top of the cell.
    pub baseline: u32,
}

/// Which part of a glyph a slot holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum GlyphPart {
    /// A single-width glyph.
    Single,
    /// The left half of a double-width glyph.
    Left,
    /// The right half of a double-width glyph.
    Right,
}

/// What identifies a slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct GlyphKey {
    /// The character.
    pub ch: char,
    /// [`BOLD`] and [`ITALIC`] bits.
    pub style: u8,
    /// Which part.
    pub part: GlyphPart,
}

/// Turns characters into coverage.
pub trait Rasterizer {
    /// The cell size every glyph is rasterized for.
    fn metrics(&self) -> Metrics;
    /// Rasterize `ch` into `out`, row-major with a stride of `columns *
    /// cell_w` bytes and `cell_h` rows, 0 = background, 255 = full ink.
    /// `columns` is 1, or 2 for a wide glyph. `out` arrives zeroed.
    fn rasterize(&mut self, ch: char, style: u8, columns: u32, out: &mut [u8]);
}

/// A deterministic rasterizer for tests and benchmarks that must not depend
/// on installed fonts: coverage is a hash of the character, style and pixel,
/// with roughly half the pixels empty. A space is blank.
#[derive(Debug, Clone, Copy)]
pub struct TestRasterizer {
    /// The metrics it reports.
    pub metrics: Metrics,
}

impl Rasterizer for TestRasterizer {
    fn metrics(&self) -> Metrics {
        self.metrics
    }

    fn rasterize(&mut self, ch: char, style: u8, columns: u32, out: &mut [u8]) {
        if ch == ' ' {
            return;
        }
        let w = self.metrics.cell_w * columns;
        for y in 0..self.metrics.cell_h {
            for x in 0..w {
                let mut h = (ch as u32).wrapping_mul(0x9E37_79B1)
                    ^ (style as u32) << 24
                    ^ x.wrapping_mul(0x85EB_CA6B)
                    ^ y.wrapping_mul(0xC2B2_AE35);
                h ^= h >> 15;
                h = h.wrapping_mul(0x2C1B_3C6D);
                h ^= h >> 12;
                let v = (h & 0x1FF) as i32 - 256;
                out[(y * w + x) as usize] = v.clamp(0, 255) as u8;
            }
        }
    }
}

/// The atlas.
pub struct Atlas {
    metrics: Metrics,
    width: u32,
    height: u32,
    slots_x: u32,
    capacity: u32,
    pixels: Vec<u8>,
    map: HashMap<GlyphKey, u32>,
    /// Slot + 1 for a single-width printable ASCII character, by style
    /// bits then character: the cells of almost every row, found without
    /// hashing. 0: not rasterized yet.
    ascii: Box<[[u32; 128]; 4]>,
    next: u32,
    misses: u64,
    /// Pixel rows written since the last [`Atlas::take_dirty`].
    dirty: Option<Range<u32>>,
    scratch: Vec<u8>,
}

impl Atlas {
    /// An empty atlas of `width x height` pixels for glyphs of `metrics`.
    ///
    /// # Panics
    /// If the atlas cannot hold at least slot 0 plus one wide glyph.
    pub fn new(metrics: Metrics, width: u32, height: u32) -> Atlas {
        assert!(metrics.cell_w > 0 && metrics.cell_h > 0);
        let slots_x = width / metrics.cell_w;
        let capacity = slots_x * (height / metrics.cell_h);
        assert!(capacity >= 3, "atlas too small for its cells");
        Atlas {
            metrics,
            width,
            height,
            slots_x,
            capacity,
            pixels: vec![0; (width * height) as usize],
            map: HashMap::new(),
            ascii: Box::new([[0; 128]; 4]),
            next: 1,
            misses: 0,
            dirty: None,
            scratch: Vec::new(),
        }
    }

    /// Cell metrics.
    pub fn metrics(&self) -> Metrics {
        self.metrics
    }

    /// Width in pixels.
    pub fn width(&self) -> u32 {
        self.width
    }

    /// Height in pixels.
    pub fn height(&self) -> u32 {
        self.height
    }

    /// Coverage, row-major, `width` bytes per row.
    pub fn pixels(&self) -> &[u8] {
        &self.pixels
    }

    /// Slots in use, including the blank slot 0.
    pub fn used(&self) -> u32 {
        self.next
    }

    /// Glyphs that did not fit and draw as blank.
    pub fn misses(&self) -> u64 {
        self.misses
    }

    /// Top-left pixel of `slot`.
    pub fn slot_origin(&self, slot: u32) -> (u32, u32) {
        (
            (slot % self.slots_x) * self.metrics.cell_w,
            (slot / self.slots_x) * self.metrics.cell_h,
        )
    }

    /// Slots per atlas row; a backend needs it to find a slot on the GPU.
    pub fn slots_per_row(&self) -> u32 {
        self.slots_x
    }

    /// The pixel rows changed since the last call, for a backend to upload.
    pub fn take_dirty(&mut self) -> Option<Range<u32>> {
        self.dirty.take()
    }

    /// The slot for `key`, rasterizing it on first use. A `Left` or `Right`
    /// key rasterizes the whole wide glyph and fills both halves.
    pub fn slot(&mut self, key: GlyphKey, raster: &mut dyn Rasterizer) -> u32 {
        let plain = (key.ch as u32) < 128 && key.part == GlyphPart::Single && key.style < 4;
        if plain {
            let s = self.ascii[key.style as usize][key.ch as usize];
            if s != 0 {
                return s - 1;
            }
        } else if let Some(&s) = self.map.get(&key) {
            return s;
        }
        let wide = key.part != GlyphPart::Single;
        let need = if wide { 2 } else { 1 };
        if self.next + need > self.capacity {
            self.misses += 1;
            return 0;
        }
        let (cw, ch) = (self.metrics.cell_w, self.metrics.cell_h);
        let w = cw * need;
        self.scratch.clear();
        self.scratch.resize((w * ch) as usize, 0);
        raster.rasterize(key.ch, key.style, need, &mut self.scratch);
        let first = self.next;
        self.next += need;
        for half in 0..need {
            let slot = first + half;
            let (ox, oy) = self.slot_origin(slot);
            for y in 0..ch {
                let src = (y * w + half * cw) as usize;
                let dst = ((oy + y) * self.width + ox) as usize;
                self.pixels[dst..dst + cw as usize]
                    .copy_from_slice(&self.scratch[src..src + cw as usize]);
            }
            let rows = oy..oy + ch;
            self.dirty = Some(match self.dirty.take() {
                None => rows,
                Some(d) => d.start.min(rows.start)..d.end.max(rows.end),
            });
        }
        if wide {
            let mut k = key;
            k.part = GlyphPart::Left;
            self.map.insert(k, first);
            k.part = GlyphPart::Right;
            self.map.insert(k, first + 1);
            return if key.part == GlyphPart::Left {
                first
            } else {
                first + 1
            };
        }
        if plain {
            self.ascii[key.style as usize][key.ch as usize] = first + 1;
        } else {
            self.map.insert(key, first);
        }
        first
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const M: Metrics = Metrics {
        cell_w: 8,
        cell_h: 16,
        baseline: 12,
    };

    fn key(ch: char, part: GlyphPart) -> GlyphKey {
        GlyphKey { ch, style: 0, part }
    }

    #[test]
    fn a_glyph_is_rasterized_once_and_marks_its_rows_dirty() {
        let mut r = TestRasterizer { metrics: M };
        let mut a = Atlas::new(M, 64, 64);
        let s = a.slot(key('x', GlyphPart::Single), &mut r);
        assert_eq!(s, 1);
        assert_eq!(a.take_dirty(), Some(0..16));
        assert_eq!(a.slot(key('x', GlyphPart::Single), &mut r), 1);
        assert_eq!(a.take_dirty(), None, "a hit uploads nothing");
        let (ox, oy) = a.slot_origin(1);
        let mut expect = vec![0u8; 8 * 16];
        r.rasterize('x', 0, 1, &mut expect);
        for y in 0..16 {
            let row = ((oy + y) * 64 + ox) as usize;
            assert_eq!(&a.pixels()[row..row + 8], &expect[(y * 8) as usize..][..8]);
        }
    }

    #[test]
    fn ascii_and_other_glyphs_keep_distinct_stable_slots_per_style() {
        let mut r = TestRasterizer { metrics: M };
        let mut a = Atlas::new(M, 128, 128);
        let plain = a.slot(key('x', GlyphPart::Single), &mut r);
        let bold = a.slot(
            GlyphKey {
                ch: 'x',
                style: BOLD,
                part: GlyphPart::Single,
            },
            &mut r,
        );
        let both = a.slot(
            GlyphKey {
                ch: 'x',
                style: BOLD | ITALIC,
                part: GlyphPart::Single,
            },
            &mut r,
        );
        let other = a.slot(key('é', GlyphPart::Single), &mut r);
        let slots = [plain, bold, both, other];
        assert!(slots.iter().all(|&s| s != 0), "{slots:?}");
        assert_eq!(
            slots.iter().collect::<std::collections::HashSet<_>>().len(),
            4
        );
        // Asked again, each comes back the same without rasterizing.
        a.take_dirty();
        assert_eq!(a.slot(key('x', GlyphPart::Single), &mut r), plain);
        assert_eq!(
            a.slot(
                GlyphKey {
                    ch: 'x',
                    style: BOLD | ITALIC,
                    part: GlyphPart::Single,
                },
                &mut r
            ),
            both
        );
        assert_eq!(a.slot(key('é', GlyphPart::Single), &mut r), other);
        assert_eq!(a.take_dirty(), None);
        assert_eq!(a.used(), 5);
    }

    #[test]
    fn a_wide_glyph_fills_two_slots_from_one_raster() {
        let mut r = TestRasterizer { metrics: M };
        let mut a = Atlas::new(M, 64, 64);
        let right = a.slot(key('界', GlyphPart::Right), &mut r);
        let left = a.slot(key('界', GlyphPart::Left), &mut r);
        assert_eq!((left, right), (1, 2));
        assert_eq!(a.used(), 3);
        let mut whole = vec![0u8; 16 * 16];
        r.rasterize('界', 0, 2, &mut whole);
        let (rx, ry) = a.slot_origin(right);
        for y in 0..16 {
            let row = ((ry + y) * 64 + rx) as usize;
            assert_eq!(
                &a.pixels()[row..row + 8],
                &whole[(y * 16 + 8) as usize..][..8]
            );
        }
    }

    #[test]
    fn a_full_atlas_draws_blank_and_counts_misses() {
        let mut r = TestRasterizer { metrics: M };
        let mut a = Atlas::new(M, 16, 32); // 4 slots
        for ch in ['a', 'b', 'c'] {
            assert_ne!(a.slot(key(ch, GlyphPart::Single), &mut r), 0);
        }
        assert_eq!(a.slot(key('d', GlyphPart::Single), &mut r), 0);
        assert_eq!(a.misses(), 1);
    }
}
