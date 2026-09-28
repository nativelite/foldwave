//! foldwave's M3 renderer spike.
//!
//! A terminal screen is drawn in three steps, each its own module:
//!
//! 1. [`Frame`] turns an [`ansi::Screen`] into one [`CellInstance`] per cell,
//!    with colours resolved and glyphs looked up in the atlas, and records which
//!    rows changed since the last frame (the *damage*).
//! 2. [`Atlas`] holds every glyph rasterized so far, once, as 8-bit coverage in
//!    cell-sized slots. A [`Rasterizer`] fills a slot on first use.
//! 3. A backend draws the frame. [`CpuRenderer`] is the portable software path
//!    and the **reference**: every other backend must produce the same pixels
//!    (within one step of rounding per channel).
//!
//! The pixel rule every backend implements, for a cell at `(row, col)` and a
//! pixel `(x, y)` inside it (`0 <= x < cell_w`, `0 <= y < cell_h`):
//!
//! * `a` = the glyph's coverage at `(x, y)`; 255 on the underline row
//!   (`y == cell_h - 2`, when the cell is flagged [`UNDERLINE`] and
//!   `cell_h >= 3`) and on the strike row (`y == cell_h / 2`, [`STRIKE`]);
//! * each colour channel = `(bg * (255 - a) + fg * a + 127) / 255`, alpha 255;
//! * the cell sits at pixel `(col * cell_w, row * cell_h)`; everything outside
//!   the grid is [`Frame::background`].
//!
//! Colours are `0xAARRGGBB`, the byte order of a Windows DIB and of a
//! `B8G8R8A8` image on a little-endian machine.
//!
//! Zero third-party dependencies.

#![forbid(unsafe_op_in_unsafe_fn)]

mod atlas;
mod catalog;
mod cpu;
mod font;
mod frame;
pub mod spirv;
#[cfg(windows)]
pub mod vk;
#[cfg(windows)]
pub mod win;

pub use atlas::{Atlas, GlyphKey, GlyphPart, Metrics, Rasterizer, TestRasterizer, BOLD, ITALIC};
pub use catalog::{
    builtin_default, font_dirs, Family, FontCatalog, JETBRAINS_MONO, JETBRAINS_MONO_ITALIC,
    JETBRAINS_MONO_LICENSE,
};
pub use cpu::{blend, CpuRenderer};
pub use font::{default_fallbacks, system_font_dir, FontFile, FontRasterizer, Sharpening};
pub use frame::{CellInstance, Frame, Highlight, Palette, STRIKE, UNDERLINE};
