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
//! There is no TrueType hinting. [`Sharpening`] instead offers cheap steps
//! toward crisp small text, each optional: fit the baseline, x-height and cap
//! height to whole pixel rows, shift each glyph so its left edge sits on a
//! pixel column, and a gamma curve on coverage. [`Sharpening::OFF`] draws the
//! raw outline.

use crate::atlas::{Metrics, Rasterizer, BOLD, ITALIC};
use glyphs::{Canvas, Font, VerticalFit};
use std::io;
use std::path::PathBuf;

/// A face with the scale that maps its units to the requested size.
struct Face {
    font: Font,
    scale: f32,
    fit: VerticalFit,
    /// Horizontal overstrike, in pixels, that takes this face's stem (the
    /// width of `|`) up to the next whole pixel; 0 when it already is one.
    stem_widen: f32,
}

impl Face {
    fn load(file: &FontFile, px_per_em: f32) -> io::Result<Face> {
        let path = &file.path;
        let data = match file.embedded {
            Some(bytes) => bytes.to_vec(),
            None => std::fs::read(path)?,
        };
        let mut font = Font::from_bytes(data, file.index).map_err(|e| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("{}: {e}", path.display()),
            )
        })?;
        font.set_variations(&file.variations);
        let scale = px_per_em / font.units_per_em() as f32;
        let fit = VerticalFit::new(&font, px_per_em);
        // `|` is a single vertical stem in most fonts: its width is the stem.
        let stem = font
            .glyph_index('|')
            .and_then(|g| font.bbox(g))
            .map(|b| (b.x_max - b.x_min) as f32 * scale)
            .filter(|&w| w > 0.0 && w < px_per_em / 3.0);
        let stem_widen = stem.map_or(0.0, |w| {
            let d = w.ceil().max(1.0) - w;
            // Already whole (or nearly): nothing to do.
            if d < 0.05 || d > 0.95 {
                0.0
            } else {
                d
            }
        });
        Ok(Face {
            font,
            scale,
            fit,
            stem_widen,
        })
    }
}

/// A font file and, for a collection, which font in it; for a variable
/// font, the instance to draw.
#[derive(Debug, Clone)]
pub struct FontFile {
    /// Path to a `.ttf` or `.ttc`; for a font compiled in, a name to show.
    pub path: PathBuf,
    /// Font index inside a collection; 0 for a single font.
    pub index: u32,
    /// Axis settings for a variable font, e.g. `[(*b"wght", 700.0)]`; empty
    /// for its default instance (and for any font that does not vary).
    pub variations: Vec<([u8; 4], f32)>,
    /// A font compiled into the program ([`crate::builtin_default`]): its
    /// bytes, read instead of `path`.
    pub embedded: Option<&'static [u8]>,
}

impl FontFile {
    /// Font 0 of `path`.
    pub fn new(path: impl Into<PathBuf>) -> FontFile {
        FontFile {
            path: path.into(),
            index: 0,
            variations: Vec::new(),
            embedded: None,
        }
    }

    /// A font compiled into the program, shown as `built in/<name>`.
    pub fn builtin(name: &str, bytes: &'static [u8]) -> FontFile {
        FontFile {
            path: PathBuf::from("built in").join(name),
            index: 0,
            variations: Vec::new(),
            embedded: Some(bytes),
        }
    }
}

/// The system font directory (`%WINDIR%\Fonts` on Windows).
pub fn system_font_dir() -> PathBuf {
    std::env::var_os("WINDIR")
        .map(|w| PathBuf::from(w).join("Fonts"))
        .unwrap_or_else(|| PathBuf::from("/usr/share/fonts"))
}

/// Fonts tried, in order, for characters the main font lacks: MS Gothic
/// (Japanese), Malgun Gothic (Korean) and Segoe UI Symbol, from the system
/// font directory.
pub fn default_fallbacks() -> Vec<FontFile> {
    let dir = system_font_dir();
    ["msgothic.ttc", "malgun.ttf", "seguisym.ttf"]
        .iter()
        .map(|n| FontFile::new(dir.join(n)))
        .collect()
}

/// How much work goes into crisp small text. Each step is cheap and
/// independent; [`Sharpening::OFF`] is the raw, exact outline.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Sharpening {
    /// Snap baseline, x-height and cap height to whole pixel rows
    /// ([`glyphs::VerticalFit`]).
    pub vertical_fit: bool,
    /// Shift each glyph (by under half a pixel) so its left edge lies on a
    /// pixel column, so the left stems are crisp.
    pub align_stems: bool,
    /// Coverage `c` becomes `c^gamma`: below 1 thickens light strokes, 1 is
    /// neutral.
    pub gamma: f32,
    /// Widen vertical strokes to whole pixels: the glyph is drawn a second
    /// time, shifted right by what the font's stem (the width of `|`) lacks
    /// of the next whole pixel, and the two copies merge. With
    /// `align_stems`, both edges of a stem then fall on pixel columns. Costs
    /// one more outline pass.
    pub snap_stems: bool,
}

impl Sharpening {
    /// The raw outline, nothing adjusted.
    pub const OFF: Sharpening = Sharpening {
        vertical_fit: false,
        align_stems: false,
        gamma: 1.0,
        snap_stems: false,
    };
}

impl Default for Sharpening {
    /// Vertical fit, stem alignment and stem snapping, neutral gamma: the
    /// best measured in `fwbench glyphs` (Consolas 16 px: grey share of ink
    /// 39.6 % against 70.7 % raw and GDI's 32.1 %, at about GDI's cost per
    /// glyph). Gamma added nothing visible once stems snap.
    fn default() -> Self {
        Sharpening {
            vertical_fit: true,
            align_stems: true,
            gamma: 1.0,
            snap_stems: true,
        }
    }
}

/// Rasterizes cells with TrueType fonts.
pub struct FontRasterizer {
    /// Regular, bold, italic, bold italic (by the `BOLD | ITALIC` bits).
    styles: [Option<Face>; 4],
    fallbacks: Vec<Face>,
    metrics: Metrics,
    canvas: Canvas,
    sharpening: Sharpening,
    /// `sharpening.gamma` as a table; `None` when neutral.
    curve: Option<[u8; 256]>,
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
        let main = Face::load(regular, px_per_em)?;
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
                *slot = Some(Face::load(file, px_per_em)?);
            }
        }
        let fallbacks = fallbacks
            .iter()
            .filter_map(|f| Face::load(f, px_per_em).ok())
            .collect();
        Ok(FontRasterizer {
            styles: faces,
            fallbacks,
            metrics,
            canvas: Canvas::new(0, 0),
            sharpening: Sharpening::default(),
            curve: None,
        })
    }

    /// Consolas with its bold and italic faces and the [`default_fallbacks`],
    /// from the system font directory.
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
            &default_fallbacks(),
            px_per_em,
        )
    }

    /// An installed family ([`crate::FontCatalog::family`]) with the
    /// [`default_fallbacks`].
    pub fn from_family(family: &crate::Family, px_per_em: f32) -> io::Result<FontRasterizer> {
        // A variable family without bold files: bold is its own heavier
        // instance (glyphs draws `wght` 700; a font without the axis is
        // unchanged by it).
        let heavier = |f: &FontFile| {
            let mut f = f.clone();
            f.variations.push((*b"wght", 700.0));
            f
        };
        let bold = family
            .bold
            .clone()
            .or_else(|| family.variable.then(|| heavier(&family.regular)));
        let bold_italic = family.bold_italic.clone().or_else(|| {
            family
                .italic
                .as_ref()
                .filter(|_| family.variable)
                .map(heavier)
        });
        FontRasterizer::new(
            &family.regular,
            [bold.as_ref(), family.italic.as_ref(), bold_italic.as_ref()],
            &default_fallbacks(),
            px_per_em,
        )
    }

    /// Change how glyphs are sharpened. Glyphs already in an atlas keep
    /// their old look; start a new atlas to redraw them.
    pub fn set_sharpening(&mut self, sharpening: Sharpening) {
        self.sharpening = sharpening;
        let g = sharpening.gamma;
        self.curve = (g.is_finite() && g > 0.0 && (g - 1.0).abs() > 1e-6).then(|| {
            let mut t = [0u8; 256];
            for (i, v) in t.iter_mut().enumerate() {
                *v = (255.0 * (i as f32 / 255.0).powf(g)).round() as u8;
            }
            t
        });
    }

    /// The current sharpening.
    pub fn sharpening(&self) -> Sharpening {
        self.sharpening
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
        // Box drawing, blocks, braille and powerline arrows are drawn in
        // code to fill the cell exactly, whatever the font (`boxdraw`).
        let (w, h) = (
            (self.metrics.cell_w * columns) as usize,
            self.metrics.cell_h as usize,
        );
        if crate::boxdraw::draw(ch, w, h, out) {
            return;
        }
        let FontRasterizer {
            styles,
            fallbacks,
            metrics,
            canvas,
            sharpening,
            curve,
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
        let mut pen_x = if std::ptr::eq(face, styled) && columns == 1 {
            0.0
        } else {
            ((w as f32 - advance) / 2.0).max(0.0)
        };
        if sharpening.align_stems {
            if let Some(b) = face.font.bbox(glyph) {
                let left = b.x_min as f32 * face.scale + pen_x;
                pen_x += left.round() - left;
            }
        }
        if canvas.width() != w || canvas.height() != h {
            *canvas = Canvas::new(w, h);
        } else {
            canvas.clear();
        }
        let passes: &[f32] = if sharpening.snap_stems && face.stem_widen > 0.0 {
            &[0.0, 1.0]
        } else {
            &[0.0]
        };
        let mut drawn = Ok(false);
        for &k in passes {
            let mut pen = canvas.pen(face.scale, pen_x + k * face.stem_widen, m.baseline as f32);
            drawn = if sharpening.vertical_fit {
                face.font.outline(glyph, &mut face.fit.apply(&mut pen))
            } else {
                face.font.outline(glyph, &mut pen)
            };
        }
        if matches!(drawn, Ok(true)) {
            let out = &mut out[..w * h];
            canvas.to_bytes(out);
            if let Some(t) = curve {
                out.iter_mut().for_each(|v| *v = t[*v as usize]);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_variable_family_without_bold_files_draws_bold_from_its_own_file() {
        let path = system_font_dir().join("CascadiaMono.ttf");
        if !path.exists() {
            eprintln!("CascadiaMono.ttf not installed; skipped");
            return;
        }
        let family = crate::Family {
            name: "Cascadia Mono".into(),
            regular: FontFile::new(&path),
            bold: None,
            italic: None,
            bold_italic: None,
            variable: true,
        };
        let mut r = FontRasterizer::from_family(&family, 32.0).unwrap();
        let m = r.metrics();
        let mut raster = |style| {
            let mut out = vec![0u8; (m.cell_w * m.cell_h) as usize];
            r.rasterize('M', style, 1, &mut out);
            out.iter().map(|&x| x as u32).sum::<u32>()
        };
        let (regular, bold) = (raster(0), raster(BOLD));
        assert!(
            bold > regular * 11 / 10,
            "bold ink {bold} vs regular {regular}"
        );
    }

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
        // Sharpening changes the look but not the ink much; OFF is the raw
        // outline.
        let sharp = raster(&mut r, 'x', 0, 1);
        r.set_sharpening(Sharpening::OFF);
        let raw = raster(&mut r, 'x', 0, 1);
        assert_ne!(sharp, raw, "fitting moves edges");
        r.set_sharpening(Sharpening {
            gamma: 0.7,
            ..Sharpening::OFF
        });
        assert!(
            ink(&raster(&mut r, 'x', 0, 1)) > ink(&raw),
            "gamma < 1 thickens"
        );
        r.set_sharpening(Sharpening::default());
        // A character no font has is blank, not a panic.
        assert!(raster(&mut r, '\u{10FFFD}', 0, 1).iter().all(|&v| v == 0));
    }
}
