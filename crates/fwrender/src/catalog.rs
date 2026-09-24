//! Finding installed fonts by family name.
//!
//! [`FontCatalog::system`] describes every font file in the system and
//! per-user font folders ([`glyphs::describe`]: a few small tables each, not
//! the whole file) and [`FontCatalog::family`] picks a family's regular,
//! bold, italic and bold italic faces by weight and slant.

use crate::font::{system_font_dir, FontFile};
use glyphs::FaceInfo;
use std::io;
use std::path::PathBuf;

/// A family's files, one per style; only regular is required.
#[derive(Debug, Clone)]
pub struct Family {
    /// The family name as the font spells it.
    pub name: String,
    pub regular: FontFile,
    pub bold: Option<FontFile>,
    pub italic: Option<FontFile>,
    pub bold_italic: Option<FontFile>,
    /// The regular face is a variable font; its other weights are not
    /// drawn (the default instance stands in for them).
    pub variable: bool,
}

/// The folders fonts are installed in: the system's, then the current
/// user's (`%LOCALAPPDATA%\Microsoft\Windows\Fonts`, Windows 10 1809+).
pub fn font_dirs() -> Vec<PathBuf> {
    let mut dirs = vec![system_font_dir()];
    if let Some(local) = std::env::var_os("LOCALAPPDATA") {
        dirs.push(PathBuf::from(local).join("Microsoft\\Windows\\Fonts"));
    }
    dirs
}

/// Every font found, with its description.
#[derive(Debug, Clone, Default)]
pub struct FontCatalog {
    faces: Vec<(FontFile, FaceInfo)>,
}

impl FontCatalog {
    /// Describe the fonts in `dirs` (`.ttf`, `.ttc`, `.otf`, `.otc`, not
    /// recursing). Files that cannot be read or parsed are skipped.
    pub fn scan(dirs: &[PathBuf]) -> FontCatalog {
        let mut faces = Vec::new();
        for dir in dirs {
            let Ok(entries) = std::fs::read_dir(dir) else {
                continue;
            };
            for path in entries.flatten().map(|e| e.path()) {
                let font = path
                    .extension()
                    .and_then(|e| e.to_str())
                    .map(|e| e.to_ascii_lowercase())
                    .map_or(false, |e| {
                        matches!(e.as_str(), "ttf" | "ttc" | "otf" | "otc")
                    });
                if !font {
                    continue;
                }
                let described = std::fs::File::open(&path)
                    .map(io::BufReader::new)
                    .and_then(|mut f| glyphs::describe(&mut f));
                for info in described.into_iter().flatten() {
                    let file = FontFile {
                        path: path.clone(),
                        index: info.index,
                    };
                    faces.push((file, info));
                }
            }
        }
        FontCatalog { faces }
    }

    /// The installed fonts ([`font_dirs`]).
    pub fn system() -> FontCatalog {
        FontCatalog::scan(&font_dirs())
    }

    /// A catalog of given faces (for tests and tools).
    pub fn from_faces(faces: Vec<(FontFile, FaceInfo)>) -> FontCatalog {
        FontCatalog { faces }
    }

    /// How many faces were found.
    pub fn len(&self) -> usize {
        self.faces.len()
    }

    /// No faces were found.
    pub fn is_empty(&self) -> bool {
        self.faces.is_empty()
    }

    /// The family `name` (any case), with a face per style: regular is the
    /// upright face nearest weight 400 below 600, bold the upright face
    /// nearest 700 at 600 or above, and likewise italic and bold italic.
    /// Only TrueType-outline faces count; the error says why none did.
    pub fn family(&self, name: &str) -> io::Result<Family> {
        let want = name.trim();
        let all: Vec<&(FontFile, FaceInfo)> = self
            .faces
            .iter()
            .filter(|(_, f)| f.family.eq_ignore_ascii_case(want))
            .collect();
        let not_found = |why: String| io::Error::new(io::ErrorKind::NotFound, why);
        if all.is_empty() {
            return Err(not_found(format!("no font family named {want:?}")));
        }
        let usable: Vec<_> = all.into_iter().filter(|(_, f)| f.truetype).collect();
        if usable.is_empty() {
            return Err(not_found(format!(
                "{want:?} has only CFF (PostScript) outlines, which are not supported yet"
            )));
        }
        let pick = |bold: bool, italic: bool| {
            let target: i32 = if bold { 700 } else { 400 };
            usable
                .iter()
                .filter(|(_, f)| f.italic == italic && (f.weight >= 600) == bold)
                .min_by_key(|(_, f)| (i32::from(f.weight) - target).abs())
                .map(|(file, f)| (file.clone(), f.variable, f.family.clone()))
        };
        let (regular, variable, family) = pick(false, false)
            .ok_or_else(|| not_found(format!("{want:?} has no upright regular-weight face")))?;
        Ok(Family {
            name: family,
            regular,
            bold: pick(true, false).map(|p| p.0),
            italic: pick(false, true).map(|p| p.0),
            bold_italic: pick(true, true).map(|p| p.0),
            variable,
        })
    }

    /// The monospaced families that can be drawn (TrueType outlines), sorted
    /// and without repeats.
    pub fn monospace_families(&self) -> Vec<String> {
        let mut names: Vec<String> = self
            .faces
            .iter()
            .filter(|(_, f)| f.monospace && f.truetype)
            .map(|(_, f)| f.family.clone())
            .collect();
        names.sort_by_key(|n| n.to_ascii_lowercase());
        names.dedup_by(|a, b| a.eq_ignore_ascii_case(b));
        names
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn face(
        file: &str,
        family: &str,
        weight: u16,
        italic: bool,
        truetype: bool,
    ) -> (FontFile, FaceInfo) {
        (
            FontFile::new(file),
            FaceInfo {
                index: 0,
                family: family.into(),
                style: String::new(),
                weight,
                italic,
                monospace: family != "Prop",
                truetype,
                variable: false,
            },
        )
    }

    fn catalog() -> FontCatalog {
        FontCatalog::from_faces(vec![
            face("mono-light.ttf", "Mono", 300, false, true),
            face("mono.ttf", "Mono", 400, false, true),
            face("mono-semibold.ttf", "Mono", 600, false, true),
            face("mono-bold.ttf", "Mono", 700, false, true),
            face("mono-black.ttf", "Mono", 900, false, true),
            face("mono-italic.ttf", "Mono", 400, true, true),
            face("prop.ttf", "Prop", 400, false, true),
            face("cff.otf", "Postscript Mono", 400, false, false),
            face("slanted.ttf", "Slanted", 400, true, true),
        ])
    }

    #[test]
    fn styles_are_picked_by_weight_and_slant() {
        let f = catalog().family("mono").unwrap();
        assert_eq!(f.name, "Mono");
        assert_eq!(f.regular.path, PathBuf::from("mono.ttf"));
        assert_eq!(f.bold.unwrap().path, PathBuf::from("mono-bold.ttf"));
        assert_eq!(f.italic.unwrap().path, PathBuf::from("mono-italic.ttf"));
        assert!(f.bold_italic.is_none(), "no bold italic face");
        let p = catalog().family(" PROP ").unwrap();
        assert!(p.bold.is_none() && p.italic.is_none());
    }

    #[test]
    fn missing_and_undrawable_families_say_why() {
        let c = catalog();
        let e = c.family("Nope").unwrap_err().to_string();
        assert!(e.contains("no font family named"), "{e}");
        let e = c.family("postscript mono").unwrap_err().to_string();
        assert!(e.contains("CFF"), "{e}");
        let e = c.family("Slanted").unwrap_err().to_string();
        assert!(e.contains("no upright"), "{e}");
        assert_eq!(c.monospace_families(), ["Mono", "Slanted"]);
    }

    #[test]
    fn the_installed_consolas_is_found_with_all_four_styles() {
        let dir = system_font_dir();
        if !dir.join("consola.ttf").exists() {
            eprintln!("Consolas not installed; skipped");
            return;
        }
        let c = FontCatalog::scan(&[dir.clone()]);
        let f = c.family("Consolas").unwrap();
        let name = |f: &Option<FontFile>| {
            f.as_ref()
                .and_then(|f| f.path.file_name())
                .map(|n| n.to_string_lossy().to_ascii_lowercase())
        };
        assert_eq!(
            name(&Some(f.regular.clone())).as_deref(),
            Some("consola.ttf")
        );
        assert_eq!(name(&f.bold).as_deref(), Some("consolab.ttf"));
        assert_eq!(name(&f.italic).as_deref(), Some("consolai.ttf"));
        assert_eq!(name(&f.bold_italic).as_deref(), Some("consolaz.ttf"));
        assert!(c.monospace_families().iter().any(|n| n == "Consolas"));
    }
}
