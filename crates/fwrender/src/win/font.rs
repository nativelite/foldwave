//! [`GdiRasterizer`]: glyph coverage from an installed font through GDI.

use super::sys::*;
use crate::atlas::{Metrics, Rasterizer, BOLD, ITALIC};
use std::ffi::c_void;
use std::io;

/// A [`Rasterizer`] backed by GDI.
///
/// Each glyph is drawn white-on-black with grayscale antialiasing
/// (`ANTIALIASED_QUALITY`, not ClearType, so the three colour channels carry
/// the same value) into a 32bpp top-down DIB section two cells wide, and the
/// green channel is copied out as coverage. Text is clipped to the requested
/// cell width with `ETO_CLIPPED`, so ink never lands outside the slot.
///
/// The four faces (regular, bold, italic, bold italic) are created up front.
/// GDI substitutes the closest installed font when `face` is missing, so the
/// face you ask for is not guaranteed to be the face you get.
///
/// The memory DC and fonts are released on drop. Holds raw GDI handles, so it
/// is neither `Send` nor `Sync`.
#[derive(Debug)]
pub struct GdiRasterizer {
    dc: Hdc,
    bitmap: Hgdiobj,
    old_bitmap: Hgdiobj,
    /// First pixel of the DIB: `dib_w * dib_h` `u32`s, owned by `bitmap`.
    bits: *mut u32,
    dib_w: u32,
    dib_h: u32,
    /// Indexed by the style bits: 0 regular, 1 bold, 2 italic, 3 bold italic.
    fonts: [Hgdiobj; 4],
    metrics: Metrics,
}

fn gdi_err(what: &str) -> io::Error {
    io::Error::new(io::ErrorKind::Other, format!("{what} failed"))
}

impl GdiRasterizer {
    /// Open `face` at `pixel_height` pixels (the character height, as
    /// `CreateFontW` takes a negative height).
    ///
    /// Metrics: `cell_w` is the advance of `M`; `cell_h` is `tmHeight`
    /// (ascent + descent; external leading is left out so rows sit flush,
    /// which is what box-drawing characters need); `baseline` is `tmAscent`.
    pub fn new(face: &str, pixel_height: i32) -> io::Result<GdiRasterizer> {
        let face_w = wide(face);
        if face_w.len() > LF_FACESIZE || face_w[..face_w.len() - 1].contains(&0) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "font face name must be at most 31 UTF-16 units, without NUL",
            ));
        }
        if pixel_height <= 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "pixel height must be positive",
            ));
        }

        let mut r = GdiRasterizer {
            dc: std::ptr::null_mut(),
            bitmap: std::ptr::null_mut(),
            old_bitmap: std::ptr::null_mut(),
            bits: std::ptr::null_mut(),
            dib_w: 0,
            dib_h: 0,
            fonts: [std::ptr::null_mut(); 4],
            metrics: Metrics {
                cell_w: 0,
                cell_h: 0,
                baseline: 0,
            },
        };
        // From here on every handle lives in `r`, so an early return frees it.

        // SAFETY: a null DC asks for a memory DC compatible with the screen.
        r.dc = unsafe { CreateCompatibleDC(std::ptr::null_mut()) };
        if r.dc.is_null() {
            return Err(gdi_err("CreateCompatibleDC"));
        }
        for style in 0..4u8 {
            let weight = if style & BOLD != 0 {
                FW_BOLD
            } else {
                FW_NORMAL
            };
            let italic = (style & ITALIC != 0) as u32;
            // SAFETY: `face_w` is NUL-terminated and at most LF_FACESIZE units;
            // every other argument is a plain value.
            let font = unsafe {
                CreateFontW(
                    -pixel_height,
                    0,
                    0,
                    0,
                    weight,
                    italic,
                    0,
                    0,
                    DEFAULT_CHARSET,
                    OUT_TT_PRECIS,
                    CLIP_DEFAULT_PRECIS,
                    ANTIALIASED_QUALITY,
                    FIXED_PITCH | FF_MODERN,
                    face_w.as_ptr(),
                )
            };
            if font.is_null() {
                return Err(gdi_err("CreateFontW"));
            }
            r.fonts[style as usize] = font;
        }

        // Metrics come from the regular face.
        let mut tm = TextMetricW::default();
        let mut m_size = Size::default();
        let m = [u16::from(b'M')];
        // SAFETY: `dc` and the font are live; the out-pointers are valid
        // locals of the documented types; `m` holds the one unit passed.
        let ok = unsafe {
            SelectObject(r.dc, r.fonts[0]);
            GetTextMetricsW(r.dc, &mut tm) != 0
                && GetTextExtentPoint32W(r.dc, m.as_ptr(), 1, &mut m_size) != 0
        };
        if !ok || m_size.cx <= 0 || tm.height <= 0 || tm.ascent < 0 {
            return Err(gdi_err("GetTextMetricsW / GetTextExtentPoint32W"));
        }
        r.metrics = Metrics {
            cell_w: m_size.cx as u32,
            cell_h: tm.height as u32,
            baseline: (tm.ascent as u32).min(tm.height as u32 - 1),
        };

        r.dib_w = 2 * r.metrics.cell_w;
        r.dib_h = r.metrics.cell_h;
        let info = BitmapInfo::top_down_32(r.dib_w as i32, r.dib_h as i32);
        let mut bits: *mut c_void = std::ptr::null_mut();
        // SAFETY: `info` is a valid 32bpp BI_RGB header; `bits` receives the
        // pixel pointer; no file-mapping section is used.
        r.bitmap = unsafe {
            CreateDIBSection(
                r.dc,
                &info,
                DIB_RGB_COLORS,
                &mut bits,
                std::ptr::null_mut(),
                0,
            )
        };
        if r.bitmap.is_null() || bits.is_null() {
            return Err(gdi_err("CreateDIBSection"));
        }
        r.bits = bits.cast();
        // SAFETY: `dc` and `bitmap` are live; the DC's original 1x1 bitmap
        // is kept to be selected back before deletion.
        unsafe {
            r.old_bitmap = SelectObject(r.dc, r.bitmap);
            SetTextColor(r.dc, 0x00FF_FFFF);
            SetBkMode(r.dc, TRANSPARENT);
        }
        Ok(r)
    }
}

impl Rasterizer for GdiRasterizer {
    fn metrics(&self) -> Metrics {
        self.metrics
    }

    fn rasterize(&mut self, ch: char, style: u8, columns: u32, out: &mut [u8]) {
        let w = columns.clamp(1, 2) * self.metrics.cell_w;
        let len = (self.dib_w * self.dib_h) as usize;
        // SAFETY: flush batched GDI work first so nothing lands after the
        // clear; `bits` points at the DIB's `len` pixels, which live as long
        // as `bitmap` (i.e. `self`).
        unsafe {
            GdiFlush();
            std::ptr::write_bytes(self.bits, 0, len);
        }

        let mut buf = [0u16; 2];
        let units = ch.encode_utf16(&mut buf);
        let clip = Rect {
            left: 0,
            top: 0,
            right: w as i32,
            bottom: self.metrics.cell_h as i32,
        };
        // SAFETY: `dc` is live with the DIB selected; the font handle is live;
        // `units` holds exactly `units.len()` (1 or 2) UTF-16 units; `clip`
        // outlives the call; a null `dx` means default spacing.
        unsafe {
            SelectObject(self.dc, self.fonts[(style & (BOLD | ITALIC)) as usize]);
            ExtTextOutW(
                self.dc,
                0,
                0,
                ETO_CLIPPED,
                &clip,
                units.as_ptr(),
                units.len() as u32,
                std::ptr::null(),
            );
            GdiFlush();
        }

        // SAFETY: GDI finished writing (flushed above) and makes no further
        // writes while this shared view lives; see the clear for the bounds.
        let pixels = unsafe { std::slice::from_raw_parts(self.bits, len) };
        let stride = w as usize;
        for (y, row) in out
            .chunks_exact_mut(stride)
            .take(self.metrics.cell_h as usize)
            .enumerate()
        {
            let src = &pixels[y * self.dib_w as usize..][..stride];
            for (d, s) in row.iter_mut().zip(src) {
                *d = (s >> 8) as u8; // green
            }
        }
    }
}

impl Drop for GdiRasterizer {
    fn drop(&mut self) {
        // SAFETY: every non-null handle was created by `new` and is released
        // once. The original bitmap is selected back first so neither the DIB
        // nor any font is still selected when deleted (the DC is deleted
        // before the fonts, which deselects the current one).
        unsafe {
            if !self.old_bitmap.is_null() {
                SelectObject(self.dc, self.old_bitmap);
            }
            if !self.dc.is_null() {
                DeleteDC(self.dc);
            }
            if !self.bitmap.is_null() {
                DeleteObject(self.bitmap);
            }
            for f in self.fonts {
                if !f.is_null() {
                    DeleteObject(f);
                }
            }
        }
    }
}
