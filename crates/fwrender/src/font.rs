//! Our own glyph rasterizer: TrueType fonts through `glyphs`, drawn straight
//! into cell-sized coverage.
//!
//! A [`FontRasterizer`] has a face per style (regular, bold, italic, bold
//! italic; missing styles fall back to regular) and a list of fallback fonts
//! for characters the main font lacks (CJK, symbols). The cell comes from the
//! main font: its advance of `M` wide, and tall enough for its ascent and
//! descent as Windows lays out lines (`OS/2` metrics), each rounded to whole
//! pixels, with the baseline at the rounded ascent.
//! Each glyph is drawn at its exact, unrounded pen position, so nothing is
//! lost to placing a pre-rendered bitmap. A fallback glyph whose advance
//! differs from its cells is centred in them.
//!
//! No hinting (see `glyphs`): stems are not snapped to pixels, so small sizes
//! look softer than GDI's hinted Consolas.

use crate::atlas::{Metrics, Rasterizer, BOLD, ITALIC};
use glyphs::{Canvas, Font};
use std::io;
use std::path::{Path, PathBuf};

/// A face with the scale that maps its units to the requested size.
struct Face {
    font: Font,
    scale: f32,
}

impl Face {
    fn load(path: &Path, index: u32, px_per_em: f32) -> io::Result<Face> {
        let data = std::fs::read(path)?;
        let font = Font::from_bytes(data, index).map_err(|e| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("{}: {e}", path.display()),
            )
        })?;
        let scale = px_per_em / font.units_per_em() as f32;
        Ok(Face { font, scale })
    }
}

/// A font file and, for a collection, which font in it.
#[derive(Debug, Clone)]
pub struct FontFile {
    /// Path to a `.ttf` or `.ttc`.
    pub path: PathBuf,
    /// Font index inside a collection; 0 for a single font.
    pub index: u32,
}

impl FontFile {
    /// Font 0 of `path`.
    pub fn new(path: impl Into<PathBuf>) -> FontFile {
        FontFile {
            path: path.into(),
            index: 0,
        }
    }
}

/// The system font directory (`%WINDIR%\Fonts` on Windows).
pub fn system_font_dir() -> PathBuf {
    std::env::var_os("WINDIR")
        .map(|w| PathBuf::from(w).join("Fonts"))
        .unwrap_or_else(|| PathBuf::from("/usr/share/fonts"))
}

/// Rasterizes cells with TrueType fonts.
pub struct FontRasterizer {
    /// Regular, bold, italic, bold italic (by the `BOLD | ITALIC` bits).
    styles: [Option<Face>; 4],
    fallbacks: Vec<Face>,
    metrics: Metrics,
    canvas: Canvas,
}

impl FontRasterizer {
    /// A rasterizer at `px_per_em` pixels per em. `styles` are regular, bold,
    /// italic, bold italic; only regular is required. `fallbacks` are tried in
    /// order for characters the styled face lacks; one that fails to load is
    /// skipped.
    pub fn new(
        regular: &FontFile,
        styles: [Option<&FontFile>; 3],
        fallbacks: &[FontFile],
        px_per_em: f32,
    ) -> io::Result<FontRasterizer> {
        if !(px_per_em > 0.0 && px_per_em.is_finite()) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "size must be positive",
            ));
        }
        let main = Face::load(&regular.path, regular.index, px_per_em)?;
        let f = &main.font;
        let m = f.glyph_index('M').ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidData, "the main font has no 'M'")
        })?;
        let s = main.scale;
        // Vertical layout as Windows does it, so cells match GDI's: the
        // OS/2 typo metrics when the font sets USE_TYPO_METRICS, otherwise
        // usWinAscent / usWinDescent (no gap). Without OS/2, hhea with its
        // line gap split evenly above and below.
        let (ascent, descent) = match f.os2_metrics() {
            Some(o) if o.use_typo => {
                let half_gap = o.typo_line_gap.max(0) as f32 / 2.0;
                (
                    o.typo_ascender as f32 + half_gap,
                    -(o.typo_descender as f32) + half_gap,
                )
            }
            Some(o) => (o.win_ascent as f32, o.win_descent as f32),
            None => {
                let l = f.line_metrics();
                let half_gap = l.line_gap.max(0) as f32 / 2.0;
                (
                    l.ascender as f32 + half_gap,
                    -(l.descender as f32) + half_gap,
                )
            }
        };
        let baseline = (ascent * s).round().max(0.0) as u32;
        let metrics = Metrics {
            cell_w: ((f.h_metrics(m).0 as f32 * s).round() as u32).max(1),
            cell_h: (baseline + (descent * s).round().max(0.0) as u32).max(1),
            baseline,
        };
        let mut faces: [Option<Face>; 4] = [Some(main), None, None, None];
        for (slot, file) in faces.iter_mut().skip(1).zip(styles) {
            if let Some(file) = file {
                *slot = Some(Face::load(&file.path, file.index, px_per_em)?);
            }
        }
        let fallbacks = fallbacks
            .iter()
            .filter_map(|f| Face::load(&f.path, f.index, px_per_em).ok())
            .collect();
        Ok(FontRasterizer {
            styles: faces,
            fallbacks,
            metrics,
            canvas: Canvas::new(0, 0),
        })
    }

    /// Consolas with its bold and italic faces, plus MS Gothic, Malgun Gothic
    /// and Segoe UI Symbol as fallbacks, from the system font directory.
    pub fn consolas(px_per_em: f32) -> io::Result<FontRasterizer> {
        let dir = system_font_dir();
        let file = |n: &str| FontFile::new(dir.join(n));
        let (b, i, z) = (
            file("consolab.ttf"),
            file("consolai.ttf"),
            file("consolaz.ttf"),
        );
        FontRasterizer::new(
            &file("consola.ttf"),
            [
                b.path.exists().then_some(&b),
                i.path.exists().then_some(&i),
                z.path.exists().then_some(&z),
            ],
            &[
                file("msgothic.ttc"),
                file("malgun.ttf"),
                file("seguisym.ttf"),
            ],
            px_per_em,
        )
    }

    /// How many fallback fonts loaded.
    pub fn fallback_count(&self) -> usize {
        self.fallbacks.len()
    }
}

/// The face for `style`, falling back towards regular.
fn styled(styles: &[Option<Face>; 4], style: u8) -> &Face {
    let want = (style & (BOLD | ITALIC)) as usize;
    [want, want & !(ITALIC as usize), want & !(BOLD as usize), 0]
        .iter()
        .find_map(|&i| styles[i].as_ref())
        .expect("regular is always loaded")
}

impl Rasterizer for FontRasterizer {
    fn metrics(&self) -> Metrics {
        self.metrics
    }

    fn rasterize(&mut self, ch: char, style: u8, columns: u32, out: &mut [u8]) {
        let FontRasterizer {
            styles,
            fallbacks,
            metrics,
            canvas,
        } = self;
        let styled = styled(styles, style);
        let (face, glyph) = match styled.font.glyph_index(ch) {
            Some(g) => (styled, g),
            None => match fallbacks
                .iter()
                .find_map(|f| f.font.glyph_index(ch).map(|g| (f, g)))
            {
                Some(found) => found,
                None => return, // no font has it: blank
            },
        };
        let m = *metrics;
        let w = (m.cell_w * columns) as usize;
        let h = m.cell_h as usize;
        // Centre a glyph whose advance differs from its cells (fallbacks,
        // wide glyphs); the main font's glyphs sit at the cell's left edge.
        let advance = face.font.h_metrics(glyph).0 as f32 * face.scale;
        let pen_x = if std::ptr::eq(face, styled) && columns == 1 {
            0.0
        } else {
            ((w as f32 - advance) / 2.0).max(0.0)
        };
        if canvas.width() != w || canvas.height() != h {
            *canvas = Canvas::new(w, h);
        } else {
            canvas.clear();
        }
        let drawn = {
            let mut pen = canvas.pen(face.scale, pen_x, m.baseline as f32);
            face.font.outline(glyph, &mut pen)
        };
        if matches!(drawn, Ok(true)) {
            canvas.to_bytes(&mut out[..w * h]);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn consolas_cells_ink_styles_and_fallbacks() {
        let mut r = match FontRasterizer::consolas(16.0) {
            Ok(r) => r,
            Err(e) => {
                eprintln!("Consolas not available ({e}); skipped");
                return;
            }
        };
        let m = r.metrics();
        assert!((7..=10).contains(&m.cell_w), "{m:?}");
        assert!((16..=21).contains(&m.cell_h), "{m:?}");
        assert!(m.baseline < m.cell_h);
        let raster = |r: &mut FontRasterizer, ch, style, cols: u32| {
            let mut out = vec![0u8; (m.cell_w * cols * m.cell_h) as usize];
            r.rasterize(ch, style, cols, &mut out);
            out
        };
        let regular = raster(&mut r, 'M', 0, 1);
        assert!(regular.iter().any(|&v| v > 200), "solid ink");
        assert!(raster(&mut r, ' ', 0, 1).iter().all(|&v| v == 0));
        let bold = raster(&mut r, 'M', BOLD, 1);
        let ink = |v: &[u8]| v.iter().map(|&x| x as u32).sum::<u32>();
        assert!(ink(&bold) > ink(&regular), "bold has more ink");
        assert_ne!(raster(&mut r, 'M', ITALIC, 1), regular);
        // Consolas has no CJK; the fallback draws it across two cells.
        if r.fallback_count() > 0 {
            let wide = raster(&mut r, '界', 0, 2);
            let half = (m.cell_w) as usize;
            let (left, right): (u32, u32) = wide.chunks(2 * half).fold((0, 0), |(l, rr), row| {
                (ink(&row[..half]) + l, ink(&row[half..]) + rr)
            });
            assert!(left > 0 && right > 0, "ink in both halves: {left} {right}");
        }
        // A character no font has is blank, not a panic.
        assert!(raster(&mut r, '\u{10FFFD}', 0, 1).iter().all(|&v| v == 0));
    }
}
