//! fwterm's colours (design §11 row 17): 未来 Mirai by default, `classic`
//! (the greys fwterm drew before), and any colour overridden in the settings
//! file, `color.<name> = #rrggbb`, so the scheme is the user's to change.

use fwrender::Palette;

/// Every colour fwterm draws with: the terminal's palette, and the window's
/// own marks around it.
#[derive(Debug, Clone, PartialEq)]
pub struct Theme {
    /// Text, background, the 256 indexed colours and the selection.
    pub palette: Palette,
    /// Hints and secondary text.
    pub dim: u32,
    /// Focus and emphasis: the focused pane's corners, `find`.
    pub accent: u32,
    /// Hairline rules: the dividers between panes.
    pub rule: u32,
    /// The search and paste bars.
    pub bar: u32,
    pub bar_text: u32,
    /// A failed command's `exit N`.
    pub error: u32,
}

/// Opaque ARGB from `0xRRGGBB`.
const fn rgb(c: u32) -> u32 {
    0xFF00_0000 | c
}

/// The theme names `--theme` and `theme =` take.
pub const NAMES: [&str; 2] = ["mirai", "classic"];

impl Theme {
    /// 未来 Mirai, foldwave's own look: ice cyan and holo lilac on black
    /// glass. Every terminal colour but black reads at 4.5:1 or better on
    /// its background.
    pub fn mirai() -> Theme {
        let mut palette = Palette::default();
        const ANSI: [u32; 16] = [
            0x0d1622, 0xff5c7a, 0x4dffb4, 0xffe066, 0x5aa9ff, 0xb9a6ff, 0x5ef2ff, 0xcfe6f2,
            0x6b8296, 0xff8aa0, 0x8affd0, 0xfff09a, 0x8cc4ff, 0xd4c8ff, 0x9ff8ff, 0xffffff,
        ];
        for (i, c) in ANSI.iter().enumerate() {
            palette.indexed[i] = rgb(*c);
        }
        palette.fg = rgb(0xdff6ff);
        palette.bg = rgb(0x060b12);
        palette.selection = rgb(0x173247);
        Theme {
            palette,
            dim: rgb(0x7f9bb0),
            accent: rgb(0x5ef2ff),
            // The accent at a quarter over the background: a hairline.
            rule: rgb(0x1c454d),
            bar: rgb(0x0f2233),
            bar_text: rgb(0xdff6ff),
            error: rgb(0xff5c7a),
        }
    }

    /// The colours fwterm drew with before Mirai.
    pub fn classic() -> Theme {
        Theme {
            palette: Palette::default(),
            dim: rgb(0x7f7f7f),
            accent: rgb(0x3a96dd),
            rule: rgb(0x4a4a4a),
            bar: rgb(0x264f78),
            bar_text: rgb(0xe8e8e8),
            error: rgb(0xd05a5a),
        }
    }

    /// A theme by name (case ignored).
    pub fn named(name: &str) -> Option<Theme> {
        match name.to_ascii_lowercase().as_str() {
            "mirai" => Some(Theme::mirai()),
            "classic" => Some(Theme::classic()),
            _ => None,
        }
    }

    /// Apply `color.<name> = #rrggbb` overrides from the settings file:
    /// `bg`, `fg`, `selection`, `dim`, `accent`, `rule`, `bar`, `bar_text`,
    /// `error`, `ansi0`..`ansi15`. Returns the keys it could not use.
    pub fn apply(&mut self, overrides: &[(String, String)]) -> Vec<String> {
        let mut bad = Vec::new();
        for (key, value) in overrides {
            let Some(c) = hex(value) else {
                bad.push(key.clone());
                continue;
            };
            let slot = match key.as_str() {
                "bg" => &mut self.palette.bg,
                "fg" => &mut self.palette.fg,
                "selection" => &mut self.palette.selection,
                "dim" => &mut self.dim,
                "accent" => &mut self.accent,
                "rule" => &mut self.rule,
                "bar" => &mut self.bar,
                "bar_text" => &mut self.bar_text,
                "error" => &mut self.error,
                k => match k.strip_prefix("ansi").and_then(|n| n.parse::<usize>().ok()) {
                    Some(n) if n < 16 => &mut self.palette.indexed[n],
                    _ => {
                        bad.push(key.clone());
                        continue;
                    }
                },
            };
            *slot = c;
        }
        bad
    }
}

/// `#rrggbb` (or `rrggbb`) as opaque ARGB.
fn hex(s: &str) -> Option<u32> {
    let h = s.trim().trim_start_matches('#');
    (h.len() == 6 && h.bytes().all(|b| b.is_ascii_hexdigit()))
        .then(|| u32::from_str_radix(h, 16).ok().map(rgb))
        .flatten()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mirai_is_the_recorded_scheme_and_classic_what_fwterm_drew_before() {
        let m = Theme::mirai();
        assert_eq!((m.palette.bg, m.palette.fg), (0xFF06_0B12, 0xFFDF_F6FF));
        assert_eq!(
            m.palette.indexed[8], 0xFF6B_8296,
            "bright black lifted to 4.9:1"
        );
        assert_eq!(
            m.palette.indexed[16],
            Palette::default().indexed[16],
            "the cube is unchanged"
        );
        let c = Theme::classic();
        assert_eq!(c.palette, Palette::default());
        assert_eq!(
            (c.bar, c.rule, c.accent),
            (0xFF26_4F78, 0xFF4A_4A4A, 0xFF3A_96DD)
        );
        assert_eq!(Theme::named("MIRAI"), Some(Theme::mirai()));
        assert_eq!(Theme::named("solarized"), None);
    }

    #[test]
    fn colours_are_overridden_by_name_and_bad_keys_reported() {
        let mut t = Theme::mirai();
        let kv = |k: &str, v: &str| (k.to_string(), v.to_string());
        let bad = t.apply(&[
            kv("bg", "#101010"),
            kv("accent", "ff8800"),
            kv("ansi1", "#AA0000"),
            kv("ansi16", "#000000"),
            kv("glow", "#123456"),
            kv("fg", "white"),
        ]);
        assert_eq!(t.palette.bg, 0xFF10_1010);
        assert_eq!(t.accent, 0xFFFF_8800);
        assert_eq!(t.palette.indexed[1], 0xFFAA_0000);
        assert_eq!(
            t.palette.fg,
            Theme::mirai().palette.fg,
            "a bad value changes nothing"
        );
        assert_eq!(bad, ["ansi16", "glow", "fg"]);
    }
}
