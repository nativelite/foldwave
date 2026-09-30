//! A Windows resource file (`.res`) for `fwterm.exe`, built by `build.rs`
//! and handed to the MSVC linker, which takes `.res` files as they are: the
//! Reticle at every size Explorer asks for, and the version information
//! Explorer and Task Manager show. The formats are Microsoft's documented
//! ones (RESOURCEHEADER, GRPICONDIR, VS_VERSIONINFO); nothing else is used.

const RT_ICON: u16 = 3;
const RT_GROUP_ICON: u16 = 14;
const RT_VERSION: u16 = 16;
/// US English, the language the version strings are tagged with.
const LANG: u16 = 0x0409;

/// Pad `v` with zero bytes to a multiple of four.
fn align(v: &mut Vec<u8>) {
    while v.len() % 4 != 0 {
        v.push(0);
    }
}

fn utf16z(s: &str) -> Vec<u8> {
    s.encode_utf16()
        .chain([0])
        .flat_map(u16::to_le_bytes)
        .collect()
}

/// One entry: its RESOURCEHEADER (type and name by number), then the data,
/// padded to four bytes.
fn entry(out: &mut Vec<u8>, kind: u16, name: u16, flags: u16, lang: u16, data: &[u8]) {
    out.extend((data.len() as u32).to_le_bytes());
    out.extend(32u32.to_le_bytes());
    for id in [kind, name] {
        out.extend(0xFFFFu16.to_le_bytes());
        out.extend(id.to_le_bytes());
    }
    out.extend(0u32.to_le_bytes()); // DataVersion
    out.extend(flags.to_le_bytes());
    out.extend(lang.to_le_bytes());
    out.extend(0u32.to_le_bytes()); // Version
    out.extend(0u32.to_le_bytes()); // Characteristics
    out.extend(data);
    align(out);
}

/// A square RGBA image (straight alpha, top row first) as an icon's DIB:
/// a BITMAPINFOHEADER of twice the height, the colour rows bottom-up in
/// BGRA, then the AND mask (set where fully transparent).
pub fn icon_dib(size: u32, rgba: &[u8]) -> Vec<u8> {
    let mask_row = (size + 31) / 32 * 4;
    let mut v = Vec::new();
    v.extend(40u32.to_le_bytes());
    v.extend((size as i32).to_le_bytes());
    v.extend((2 * size as i32).to_le_bytes());
    v.extend(1u16.to_le_bytes());
    v.extend(32u16.to_le_bytes());
    v.extend(0u32.to_le_bytes()); // BI_RGB
    v.extend((size * size * 4 + mask_row * size).to_le_bytes());
    v.extend([0u8; 16]);
    for y in (0..size).rev() {
        for x in 0..size {
            let i = ((y * size + x) * 4) as usize;
            v.extend([rgba[i + 2], rgba[i + 1], rgba[i], rgba[i + 3]]);
        }
    }
    for y in (0..size).rev() {
        let mut row = vec![0u8; mask_row as usize];
        for x in 0..size {
            if rgba[((y * size + x) * 4 + 3) as usize] == 0 {
                row[(x / 8) as usize] |= 0x80 >> (x % 8);
            }
        }
        v.extend(row);
    }
    v
}

/// A VS_VERSIONINFO node: its key, value and children, each child aligned.
/// `text` values count their length in characters, binary ones in bytes.
fn node(key: &str, value: &[u8], text: bool, children: &[Vec<u8>]) -> Vec<u8> {
    let mut v = vec![0u8; 6];
    v.extend(utf16z(key));
    align(&mut v);
    v.extend(value);
    for c in children {
        align(&mut v);
        v.extend(c);
    }
    let value_len = if text { value.len() / 2 } else { value.len() };
    let len = v.len() as u16;
    v[0..2].copy_from_slice(&len.to_le_bytes());
    v[2..4].copy_from_slice(&(value_len as u16).to_le_bytes());
    v[4..6].copy_from_slice(&u16::from(text || value.is_empty()).to_le_bytes());
    v
}

/// The version resource: `version` as `major.minor.patch`, and `strings`
/// (CompanyName, FileDescription, ...) in US English.
pub fn version_info(version: (u16, u16, u16), strings: &[(&str, &str)]) -> Vec<u8> {
    let (ma, mi, pa) = version;
    let ms = (u32::from(ma) << 16) | u32::from(mi);
    let ls = u32::from(pa) << 16;
    let mut fixed = Vec::new();
    for d in [
        0xFEEF_04BDu32, // signature
        0x0001_0000,    // structure version
        ms,
        ls,
        ms,
        ls,
        0x3F,        // flags mask
        0,           // flags
        0x0004_0004, // VOS_NT_WINDOWS32
        1,           // VFT_APP
        0,
        0,
        0,
    ] {
        fixed.extend(d.to_le_bytes());
    }
    let table: Vec<Vec<u8>> = strings
        .iter()
        .map(|(k, v)| node(k, &utf16z(v), true, &[]))
        .collect();
    let strings = node(
        "StringFileInfo",
        &[],
        true,
        &[node("040904B0", &[], true, &table)],
    );
    let translation = [LANG.to_le_bytes(), 1200u16.to_le_bytes()].concat();
    let vars = node(
        "VarFileInfo",
        &[],
        true,
        &[node("Translation", &translation, false, &[])],
    );
    node("VS_VERSION_INFO", &fixed, false, &[strings, vars])
}

/// The whole `.res` file: `icons` (size, RGBA) as icon group 1, and the
/// version resource.
pub fn res_file(icons: &[(u32, Vec<u8>)], version: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    // The empty first entry that marks a 32-bit resource file.
    entry(&mut out, 0, 0, 0, 0, &[]);
    let mut group = Vec::new();
    group.extend(0u16.to_le_bytes());
    group.extend(1u16.to_le_bytes());
    group.extend((icons.len() as u16).to_le_bytes());
    for (i, (size, rgba)) in icons.iter().enumerate() {
        let id = i as u16 + 1;
        let dib = icon_dib(*size, rgba);
        entry(&mut out, RT_ICON, id, 0x1010, LANG, &dib);
        // 256 is written as 0 in a byte.
        let side = if *size >= 256 { 0 } else { *size as u8 };
        group.extend([side, side, 0, 0]);
        group.extend(1u16.to_le_bytes());
        group.extend(32u16.to_le_bytes());
        group.extend((dib.len() as u32).to_le_bytes());
        group.extend(id.to_le_bytes());
    }
    entry(&mut out, RT_GROUP_ICON, 1, 0x1030, LANG, &group);
    entry(&mut out, RT_VERSION, 1, 0x0030, LANG, version);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn u16_at(b: &[u8], i: usize) -> u16 {
        u16::from_le_bytes([b[i], b[i + 1]])
    }

    fn u32_at(b: &[u8], i: usize) -> u32 {
        u32::from_le_bytes([b[i], b[i + 1], b[i + 2], b[i + 3]])
    }

    #[test]
    fn a_dib_is_bottom_up_bgra_with_a_mask_where_transparent() {
        // 2x2: top-left red opaque, the rest transparent.
        let mut rgba = vec![0u8; 16];
        rgba[..4].copy_from_slice(&[255, 0, 0, 255]);
        let d = icon_dib(2, &rgba);
        assert_eq!(u32_at(&d, 0), 40);
        assert_eq!(u32_at(&d, 8), 4, "twice the height: colour and mask");
        // Colour rows start at 40, bottom row first: the top row is second.
        assert_eq!(&d[48..52], &[0, 0, 255, 255], "red as BGRA");
        // Mask rows (4 bytes each) after 16 bytes of colour.
        let mask = &d[56..];
        assert_eq!(mask.len(), 8);
        assert_eq!(mask[0], 0b1100_0000, "bottom row: both transparent");
        assert_eq!(mask[4], 0b0100_0000, "top row: the right one only");
    }

    #[test]
    fn the_version_resource_nests_and_aligns() {
        let v = version_info((1, 2, 3), &[("ProductName", "foldwave")]);
        assert_eq!(u16_at(&v, 0) as usize, v.len());
        assert_eq!(u16_at(&v, 2), 52, "the fixed info");
        let fixed = 40; // header, "VS_VERSION_INFO" and its NUL, aligned
        assert_eq!(u32_at(&v, fixed), 0xFEEF_04BD);
        assert_eq!(u32_at(&v, fixed + 8), 0x0001_0002);
        assert_eq!(u32_at(&v, fixed + 12), 0x0003_0000);
        let text = |s: &str| {
            s.encode_utf16()
                .flat_map(u16::to_le_bytes)
                .collect::<Vec<u8>>()
        };
        let find = |needle: &[u8]| v.windows(needle.len()).position(|w| w == needle);
        let name = find(&text("ProductName")).unwrap();
        let node = name - 6;
        assert_eq!(node % 4, 0, "each node starts aligned");
        assert_eq!(
            u16_at(&v, node + 2),
            9,
            "\"foldwave\" and its NUL, in characters"
        );
        assert!(find(&text("foldwave")).unwrap() % 4 == 0);
        assert!(find(&text("Translation")).is_some());
    }

    #[test]
    fn the_res_file_holds_every_icon_its_group_and_the_version() {
        let icons = vec![
            (16, vec![255u8; 16 * 16 * 4]),
            (256, vec![255u8; 256 * 256 * 4]),
        ];
        let res = res_file(&icons, &version_info((0, 1, 0), &[]));
        // The empty first entry.
        assert_eq!(&res[..8], &[0, 0, 0, 0, 32, 0, 0, 0]);
        // Walk the entries: (type, name, size).
        let mut at = 32;
        let mut seen = Vec::new();
        while at < res.len() {
            let size = u32_at(&res, at) as usize;
            let head = u32_at(&res, at + 4) as usize;
            seen.push((u16_at(&res, at + 10), u16_at(&res, at + 14), size));
            at += head + (size + 3) / 4 * 4;
        }
        assert_eq!(at, res.len());
        let kinds: Vec<_> = seen.iter().map(|s| (s.0, s.1)).collect();
        assert_eq!(kinds, [(3, 1), (3, 2), (14, 1), (16, 1)]);
        assert_eq!(seen[2].2, 6 + 2 * 14, "a group entry per icon");
        let group_at = 32 + 32 + seen[0].2 + 32 + seen[1].2 + 32;
        assert_eq!(res[group_at + 6 + 14], 0, "256 px written as 0");
    }
}
