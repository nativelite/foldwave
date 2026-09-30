//! Installing foldwave for one user, no administrator needed (design M5):
//! `fwterm.exe` and Microsoft's ConPTY files into
//! `%LOCALAPPDATA%\Programs\foldwave`, a Start-menu shortcut, and an entry
//! in Apps & features whose Uninstall runs `fwterm.exe --uninstall`. The
//! download is fwterm itself named `foldwave-setup-<version>.exe`, which
//! installs when started under that name.
//!
//! Files in use (a foldwave window still open) are moved aside, which
//! Windows allows for a running program's files, and swept on the next
//! install. The shortcut is written in the documented Shell Link format
//! (MS-SHLLINK), the Apps & features entry through `reg.exe import`.

use crate::settings_ui::{panel, put, Cell, Paint};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// Where the Apps & features entry lives.
pub const UNINSTALL_KEY: &str =
    r"HKEY_CURRENT_USER\Software\Microsoft\Windows\CurrentVersion\Uninstall\foldwave";

/// The files an install puts in its folder, besides fwterm.exe.
pub const CONPTY_FILES: [&str; 2] = ["conpty.dll", "OpenConsole.exe"];

/// Where an install goes.
#[derive(Debug, Clone, PartialEq)]
pub struct Places {
    pub dir: PathBuf,
    pub exe: PathBuf,
    pub shortcut: PathBuf,
    /// Where the shortcut starts fwterm: the user's profile folder.
    pub home: PathBuf,
}

/// The places for a user whose `%LOCALAPPDATA%`, `%APPDATA%` and
/// `%USERPROFILE%` are these.
pub fn places(local: &Path, roaming: &Path, home: &Path) -> Places {
    let dir = local.join("Programs").join("foldwave");
    Places {
        exe: dir.join("fwterm.exe"),
        dir,
        shortcut: roaming.join(r"Microsoft\Windows\Start Menu\Programs\foldwave.lnk"),
        home: home.to_path_buf(),
    }
}

/// Started as the setup download (`foldwave-setup-0.1.0.exe`, or a copy
/// the browser renamed, `foldwave-setup-0.1.0 (1).exe`).
pub fn is_setup(exe: &Path) -> bool {
    exe.file_stem()
        .and_then(|s| s.to_str())
        .is_some_and(|s| s.to_ascii_lowercase().starts_with("foldwave-setup"))
}

fn utf16(s: &str) -> Vec<u8> {
    s.encode_utf16().flat_map(u16::to_le_bytes).collect()
}

/// Windows' "My Computer" folder, the root of a path's item list.
const MY_COMPUTER: [u8; 16] = [
    0xE0, 0x4F, 0xD0, 0x20, 0xEA, 0x3A, 0x69, 0x10, 0xA2, 0xD8, 0x08, 0x00, 0x2B, 0x30, 0x30, 0x9D,
];

/// A Shell Link (`.lnk`) to `target`, a path on a drive (`C:\...`), that
/// starts in `workdir`, shows `icon`'s first icon and is described as
/// `name`. The target is an item list, as the shell reads it: My Computer,
/// the drive, then each folder and the file by its UTF-16 name.
pub fn shortcut(target: &str, workdir: &str, icon: &str, name: &str) -> io::Result<Vec<u8>> {
    const HAS_TARGET_ID_LIST: u32 = 0x01;
    const HAS_NAME: u32 = 0x04;
    const HAS_WORKING_DIR: u32 = 0x10;
    const HAS_ICON_LOCATION: u32 = 0x40;
    const IS_UNICODE: u32 = 0x80;
    let b = target.as_bytes();
    if b.len() < 4 || !b[0].is_ascii_alphabetic() || &b[1..3] != b":\\" {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{target}: not a path on a drive"),
        ));
    }
    let mut v = Vec::new();
    // ShellLinkHeader.
    v.extend(0x4Cu32.to_le_bytes());
    v.extend([
        0x01, 0x14, 0x02, 0x00, 0x00, 0x00, 0x00, 0x00, 0xC0, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x46,
    ]);
    let flags = HAS_TARGET_ID_LIST | HAS_NAME | HAS_WORKING_DIR | HAS_ICON_LOCATION | IS_UNICODE;
    v.extend(flags.to_le_bytes());
    v.extend(0x20u32.to_le_bytes()); // FILE_ATTRIBUTE_ARCHIVE
    v.extend([0u8; 24]); // creation, access, write times
    v.extend(0u32.to_le_bytes()); // file size
    v.extend(0i32.to_le_bytes()); // icon index
    v.extend(1u32.to_le_bytes()); // SW_SHOWNORMAL
    v.extend([0u8; 12]); // hot key, reserved

    // LinkTargetIDList: each item is its size and its bytes; a zero ends it.
    let mut items: Vec<Vec<u8>> = vec![[&[0x1F, 0x50][..], &MY_COMPUTER].concat()];
    let mut drive = vec![0x2F];
    drive.extend(&b[..3]);
    drive.resize(23, 0);
    items.push(drive);
    let parts: Vec<&str> = target[3..].split('\\').filter(|p| !p.is_empty()).collect();
    for (i, part) in parts.iter().enumerate() {
        let file = i + 1 == parts.len();
        // A folder (0x31) or file (0x32) entry, its name in UTF-16 (0x04).
        let mut item = vec![if file { 0x36 } else { 0x35 }, 0];
        item.extend([0u8; 8]); // size, modified time: filled in on use
        item.extend((if file { 0x20u16 } else { 0x10 }).to_le_bytes());
        item.extend(utf16(part));
        item.extend([0, 0]);
        items.push(item);
    }
    let list: Vec<u8> = items
        .iter()
        .flat_map(|it| {
            ((it.len() + 2) as u16)
                .to_le_bytes()
                .into_iter()
                .chain(it.clone())
        })
        .chain([0, 0])
        .collect();
    v.extend((list.len() as u16).to_le_bytes());
    v.extend(list);
    // StringData, counted UTF-16: name, working folder, icon.
    for s in [name, workdir, icon] {
        v.extend((s.encode_utf16().count() as u16).to_le_bytes());
        v.extend(utf16(s));
    }
    v.extend(0u32.to_le_bytes()); // TerminalBlock
    Ok(v)
}

/// A `.reg` file's text adding foldwave to Apps & features.
pub fn uninstall_entry(p: &Places, version: &str, kib: u64) -> String {
    let quote = |s: &str| format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""));
    let exe = p.exe.display().to_string();
    let mut t = format!("Windows Registry Editor Version 5.00\r\n\r\n[{UNINSTALL_KEY}]\r\n");
    for (k, v) in [
        ("DisplayName", "foldwave".to_string()),
        ("DisplayVersion", version.to_string()),
        ("Publisher", "nativelite".to_string()),
        ("DisplayIcon", format!("{exe},0")),
        ("InstallLocation", p.dir.display().to_string()),
        ("UninstallString", format!("\"{exe}\" --uninstall")),
        (
            "QuietUninstallString",
            format!("\"{exe}\" --uninstall --quiet"),
        ),
    ] {
        t += &format!("{}={}\r\n", quote(k), quote(&v));
    }
    for (k, v) in [("NoModify", 1), ("NoRepair", 1), ("EstimatedSize", kib)] {
        t += &format!("{}=dword:{:08x}\r\n", quote(k), v);
    }
    t
}

/// `text` as `reg.exe import` reads a version 5 file: UTF-16 with its BOM.
pub fn reg_bytes(text: &str) -> Vec<u8> {
    [0xFF, 0xFE].into_iter().chain(utf16(text)).collect()
}

/// A `.reg` file's text (as `reg.exe export` writes it) read back: UTF-16
/// with its BOM.
pub fn reg_text(bytes: &[u8]) -> String {
    let body = bytes.strip_prefix(&[0xFF, 0xFE]).unwrap_or(bytes);
    let units: Vec<u16> = body
        .chunks_exact(2)
        .map(|c| u16::from_le_bytes([c[0], c[1]]))
        .collect();
    String::from_utf16_lossy(&units)
}

/// The value `name` in an exported `.reg` file: whether it is expandable
/// (`REG_EXPAND_SZ`, written `hex(2):`) and its text. Read from the UTF-16
/// bytes the export holds, so no character is lost on the way back.
pub fn reg_value(export: &str, name: &str) -> Option<(bool, String)> {
    // Long values continue on the next line after a trailing backslash.
    let mut lines: Vec<String> = Vec::new();
    let mut joining = false;
    for line in export.lines() {
        let part = if joining { line.trim_start() } else { line };
        match (joining, lines.last_mut()) {
            (true, Some(last)) => last.push_str(part.trim_end_matches('\\')),
            _ => lines.push(part.trim_end_matches('\\').to_string()),
        }
        joining = line.ends_with('\\');
    }
    let head = format!("\"{name}\"=");
    let line = lines
        .iter()
        .find(|l| l.len() >= head.len() && l[..head.len()].eq_ignore_ascii_case(&head))?;
    let value = &line[head.len()..];
    if let Some(hex) = value.strip_prefix("hex(2):") {
        let bytes: Vec<u8> = hex
            .split(',')
            .filter_map(|b| u8::from_str_radix(b.trim(), 16).ok())
            .collect();
        let units: Vec<u16> = bytes
            .chunks_exact(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .take_while(|&u| u != 0)
            .collect();
        return Some((true, String::from_utf16_lossy(&units)));
    }
    let quoted = value.strip_prefix('"')?.strip_suffix('"')?;
    let mut out = String::new();
    let mut chars = quoted.chars();
    while let Some(c) = chars.next() {
        out.push(if c == '\\' { chars.next()? } else { c });
    }
    Some((false, out))
}

/// A `.reg` file's text setting the user's environment variable `name`,
/// expandable (`hex(2)`, UTF-16 as the registry holds it) or not.
pub fn environment_entry(name: &str, expand: bool, value: &str) -> String {
    let mut t = format!(
        "Windows Registry Editor Version 5.00\r\n\r\n[HKEY_CURRENT_USER\\Environment]\r\n\"{name}\"="
    );
    if expand {
        let hex: Vec<String> = utf16(value)
            .into_iter()
            .chain([0, 0])
            .map(|b| format!("{b:02x}"))
            .collect();
        t += "hex(2):";
        for (i, chunk) in hex.chunks(24).enumerate() {
            if i > 0 {
                t += ",\\\r\n  ";
            }
            t += &chunk.join(",");
        }
    } else {
        t += &format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\""));
    }
    t + "\r\n"
}

/// `a` and `b` name the same folder in a PATH (case, non-ASCII letters
/// too, and a trailing `\` aside).
fn same_entry(a: &str, b: &str) -> bool {
    let norm = |s: &str| s.trim().trim_end_matches('\\').to_lowercase();
    norm(a) == norm(b)
}

/// `path` (a PATH value) with `dir` at its end, or `None` if it is there.
pub fn with_dir(path: &str, dir: &str) -> Option<String> {
    if path.split(';').any(|e| same_entry(e, dir)) {
        return None;
    }
    let kept = path.trim_end_matches(';');
    Some(if kept.is_empty() {
        dir.to_string()
    } else {
        format!("{kept};{dir}")
    })
}

/// `path` without `dir`, or `None` if it was not there. Every other entry
/// is kept as written.
pub fn without_dir(path: &str, dir: &str) -> Option<String> {
    let entries: Vec<&str> = path.split(';').collect();
    let kept: Vec<&str> = entries
        .iter()
        .copied()
        .filter(|e| !same_entry(e, dir))
        .collect();
    (kept.len() != entries.len()).then(|| kept.join(";"))
}

/// Put `bytes` at `path`. The same bytes already there are left alone; a
/// different file is moved aside to `<name>.old-<stamp>` first, since a
/// file in use (a running fwterm, a loaded conpty.dll) can be renamed but
/// not overwritten.
pub fn replace(path: &Path, bytes: &[u8], stamp: u64) -> io::Result<()> {
    match fs::read(path) {
        Ok(old) if old == bytes => return Ok(()),
        Ok(_) => {
            let mut aside = path.as_os_str().to_owned();
            aside.push(format!(".old-{stamp}"));
            fs::rename(path, &aside)?;
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }
    fs::write(path, bytes)
}

/// The `*.old-*` files earlier installs moved aside in `dir`.
fn set_aside(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.contains(".old-"))
        })
        .collect()
}

/// Copy `me` (this fwterm.exe) and the ConPTY files into the install
/// folder and write the shortcut; the bytes installed. A ConPTY file not
/// carried in `payload` (a build without it) is taken from beside `me`.
pub fn install_files(
    p: &Places,
    me: &Path,
    payload: &[(&str, &[u8])],
    stamp: u64,
) -> io::Result<u64> {
    fs::create_dir_all(&p.dir)?;
    // Earlier installs' files set aside, unless still in use.
    for old in set_aside(&p.dir) {
        let _ = fs::remove_file(old);
    }
    let mut total = 0u64;
    if !same_file(me, &p.exe) {
        let bytes = fs::read(me)?;
        replace(&p.exe, &bytes, stamp)?;
    }
    total += fs::metadata(&p.exe)?.len();
    for (name, carried) in payload {
        let bytes = if carried.is_empty() {
            match me.parent().map(|d| d.join(name)).filter(|f| f.is_file()) {
                Some(beside) if !same_file(&beside, &p.dir.join(name)) => fs::read(beside)?,
                _ => continue,
            }
        } else {
            carried.to_vec()
        };
        replace(&p.dir.join(name), &bytes, stamp)?;
        total += bytes.len() as u64;
    }
    if let Some(parent) = p.shortcut.parent() {
        fs::create_dir_all(parent)?;
    }
    let exe = p.exe.display().to_string();
    fs::write(
        &p.shortcut,
        shortcut(
            &exe,
            &p.home.display().to_string(),
            &exe,
            "foldwave, the GPU-rendered agentic terminal",
        )?,
    )?;
    Ok(total)
}

fn same_file(a: &Path, b: &Path) -> bool {
    match (fs::canonicalize(a), fs::canonicalize(b)) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

/// Remove the shortcut and the install folder's own files (nothing else in
/// it), then the folder if that empties it. What could not be removed is
/// returned: `me` when it runs from there, and files still in use.
pub fn remove_files(p: &Places, me: &Path) -> Vec<PathBuf> {
    let mut left = Vec::new();
    match fs::remove_file(&p.shortcut) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => left.push(p.shortcut.clone()),
        _ => {}
    }
    let ours = [p.exe.clone()]
        .into_iter()
        .chain(CONPTY_FILES.iter().map(|n| p.dir.join(n)))
        .chain(set_aside(&p.dir));
    for f in ours {
        if same_file(&f, me) {
            left.push(f);
            continue;
        }
        match fs::remove_file(&f) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => left.push(f),
            _ => {}
        }
    }
    let _ = fs::remove_dir(&p.dir);
    left
}

/// `text` in lines of at most `width` characters, broken at spaces.
fn wrap(text: &str, width: usize) -> Vec<String> {
    let mut lines = Vec::new();
    let mut line = String::new();
    for word in text.split(' ') {
        let len = line.chars().count();
        if len > 0 && len + 1 + word.chars().count() > width {
            lines.push(std::mem::take(&mut line));
        }
        if !line.is_empty() {
            line.push(' ');
        }
        line.push_str(word);
    }
    lines.push(line);
    lines
}

/// A message panel `w x h` cells: `title` in its edge, each of `lines`
/// wrapped as a paragraph, and how to close it.
pub fn notice(title: &str, lines: &[String], w: usize, h: usize) -> Vec<Vec<Cell>> {
    let (w, h) = (w.max(40), h.max(8));
    let mut g = panel(w, h, &format!(" [ FOLDWAVE ] · {title} "));
    let mut r = 2;
    for (i, para) in lines.iter().enumerate() {
        let paint = if i == 0 { Paint::Label } else { Paint::Hint };
        for l in wrap(para, w - 6) {
            if r + 3 >= h {
                break;
            }
            put(&mut g, r, 3, &l, paint, false);
            r += 1;
        }
        r += 1;
    }
    put(&mut g, h - 2, 3, "Enter or Esc closes", Paint::Hint, false);
    g
}

#[cfg(test)]
mod tests {
    use super::*;

    fn u32_at(b: &[u8], i: usize) -> u32 {
        u32::from_le_bytes([b[i], b[i + 1], b[i + 2], b[i + 3]])
    }

    fn scratch(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "fwterm-install-{name}-{}-{}",
            std::process::id(),
            crate::logstore::now_ms()
        ));
        fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn places_and_the_setup_name() {
        let p = places(
            Path::new(r"C:\Users\a\AppData\Local"),
            Path::new(r"C:\Users\a\AppData\Roaming"),
            Path::new(r"C:\Users\a"),
        );
        assert_eq!(
            p.exe,
            PathBuf::from(r"C:\Users\a\AppData\Local\Programs\foldwave\fwterm.exe")
        );
        assert_eq!(
            p.shortcut,
            PathBuf::from(
                r"C:\Users\a\AppData\Roaming\Microsoft\Windows\Start Menu\Programs\foldwave.lnk"
            )
        );
        assert!(is_setup(Path::new(
            r"C:\Downloads\foldwave-setup-0.1.0.exe"
        )));
        assert!(is_setup(Path::new(
            r"C:\Downloads\Foldwave-Setup-0.1.0 (1).exe"
        )));
        assert!(!is_setup(Path::new(r"C:\x\fwterm.exe")));
    }

    #[test]
    fn a_shortcut_carries_its_target_folder_and_icon() {
        let target = r"C:\Users\Zoë\AppData\Local\Programs\foldwave\fwterm.exe";
        let v = shortcut(target, r"C:\Users\Zoë", target, "foldwave").unwrap();
        assert_eq!(u32_at(&v, 0), 0x4C);
        assert_eq!(u32_at(&v, 20), 0x01 | 0x04 | 0x10 | 0x40 | 0x80);
        // The item list after the 76-byte header: its size, then items.
        let list = u16::from_le_bytes([v[76], v[77]]) as usize;
        let mut at = 78;
        let mut items = Vec::new();
        loop {
            let n = u16::from_le_bytes([v[at], v[at + 1]]) as usize;
            if n == 0 {
                break;
            }
            items.push(v[at + 2..at + n].to_vec());
            at += n;
        }
        assert_eq!(at + 2, 78 + list, "the list ends where its size says");
        assert_eq!(
            items.len(),
            2 + 7,
            "computer, drive, 6 folders and the file"
        );
        assert_eq!(&items[0][..2], &[0x1F, 0x50]);
        assert_eq!(&items[1][..4], b"\x2FC:\\");
        let name = |it: &[u8]| {
            let w: Vec<u16> = it[12..]
                .chunks(2)
                .map(|c| u16::from_le_bytes([c[0], c[1]]))
                .take_while(|&c| c != 0)
                .collect();
            (it[0], String::from_utf16(&w).unwrap())
        };
        assert_eq!(name(&items[2]), (0x35, "Users".to_string()));
        assert_eq!(name(&items[3]), (0x35, "Zoë".to_string()));
        assert_eq!(name(&items[8]), (0x36, "fwterm.exe".to_string()));
        // The counted strings, then the terminal block.
        let mut at = 78 + list;
        let mut strings = Vec::new();
        for _ in 0..3 {
            let n = u16::from_le_bytes([v[at], v[at + 1]]) as usize;
            let s: Vec<u16> = v[at + 2..at + 2 + 2 * n]
                .chunks(2)
                .map(|c| u16::from_le_bytes([c[0], c[1]]))
                .collect();
            strings.push(String::from_utf16(&s).unwrap());
            at += 2 + 2 * n;
        }
        assert_eq!(strings, ["foldwave", r"C:\Users\Zoë", target]);
        assert_eq!(&v[at..], &[0, 0, 0, 0]);
        assert!(shortcut(r"\\server\share\x.exe", "", "", "").is_err());
    }

    #[test]
    fn the_uninstall_entry_escapes_paths_and_counts_kib() {
        let p = places(Path::new(r"C:\L"), Path::new(r"C:\R"), Path::new(r"C:\H"));
        let t = uninstall_entry(&p, "0.1.0", 2300);
        assert!(t.starts_with("Windows Registry Editor Version 5.00\r\n\r\n[HKEY_CURRENT_USER\\"));
        assert!(t.contains(
            "\"UninstallString\"=\"\\\"C:\\\\L\\\\Programs\\\\foldwave\\\\fwterm.exe\\\" --uninstall\"\r\n"
        ));
        assert!(t.contains("\"DisplayIcon\"=\"C:\\\\L\\\\Programs\\\\foldwave\\\\fwterm.exe,0\""));
        assert!(t.contains("\"EstimatedSize\"=dword:000008fc\r\n"));
        assert_eq!(&reg_bytes("W")[..4], &[0xFF, 0xFE, b'W', 0]);
    }

    #[test]
    fn install_copies_moves_a_changed_file_aside_and_uninstall_removes_only_its_own() {
        let root = scratch("cycle");
        let src = root.join("dl");
        fs::create_dir_all(&src).unwrap();
        let me = src.join("foldwave-setup-0.1.0.exe");
        fs::write(&me, b"new exe").unwrap();
        fs::write(src.join("OpenConsole.exe"), b"console beside").unwrap();
        let p = places(&root.join("L"), &root.join("R"), &root);
        // An older install whose exe differs, and a user's file in the folder.
        fs::create_dir_all(&p.dir).unwrap();
        fs::write(&p.exe, b"old exe").unwrap();
        fs::write(p.dir.join("notes.txt"), b"mine").unwrap();
        let payload: [(&str, &[u8]); 2] = [("conpty.dll", b"dll"), ("OpenConsole.exe", b"")];
        let n = install_files(&p, &me, &payload, 7).unwrap();
        assert_eq!(fs::read(&p.exe).unwrap(), b"new exe");
        assert_eq!(
            fs::read(p.dir.join("fwterm.exe.old-7")).unwrap(),
            b"old exe"
        );
        assert_eq!(fs::read(p.dir.join("conpty.dll")).unwrap(), b"dll");
        assert_eq!(
            fs::read(p.dir.join("OpenConsole.exe")).unwrap(),
            b"console beside",
            "not carried: taken from beside the setup"
        );
        assert_eq!(n, 7 + 3 + 14);
        assert!(p.shortcut.is_file());
        // Again: the same bytes stay, and what was set aside is swept.
        install_files(&p, &me, &payload, 8).unwrap();
        assert!(!p.dir.join("fwterm.exe.old-7").exists());
        assert!(!p.dir.join("fwterm.exe.old-8").exists());
        // Uninstall, as the installed copy: it stays for later; the user's
        // file and so the folder stay too.
        let left = remove_files(&p, &p.exe);
        assert_eq!(left, [p.exe.clone()]);
        assert!(!p.shortcut.exists() && !p.dir.join("conpty.dll").exists());
        assert!(p.dir.join("notes.txt").exists());
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn the_path_value_round_trips_through_reg_files_and_gains_or_loses_the_folder() {
        let dir = r"C:\Users\Zoë\AppData\Local\Programs\foldwave";
        let path = r"%USERPROFILE%\bin;C:\Tools\Zoë;";
        // As reg.exe export writes an expandable value: hex, continued lines.
        let written = environment_entry("Path", true, path);
        assert!(written.contains("\"Path\"=hex(2):25,00,55,00"), "{written}");
        assert!(written.contains(",\\\r\n  "), "long values continue");
        let export = format!("{written}\"TEMP\"=\"C:\\\\Temp\"\r\n");
        assert_eq!(reg_value(&export, "path"), Some((true, path.to_string())));
        assert_eq!(
            reg_value(&export, "TEMP"),
            Some((false, r"C:\Temp".to_string()))
        );
        assert_eq!(reg_value(&export, "Missing"), None);
        assert_eq!(reg_text(&reg_bytes(&export)), export);
        // Added once, removed exactly, everything else as written.
        let added = with_dir(path, dir).unwrap();
        assert_eq!(added, format!(r"%USERPROFILE%\bin;C:\Tools\Zoë;{dir}"));
        assert_eq!(with_dir(&added, &format!("{dir}\\")), None, "already there");
        assert_eq!(with_dir("", dir).unwrap(), dir);
        assert_eq!(
            without_dir(&added.to_uppercase(), dir).unwrap(),
            r"%USERPROFILE%\BIN;C:\TOOLS\ZOË"
        );
        assert_eq!(without_dir(path, dir), None, "not there: unchanged");
        let s = environment_entry("Path", false, r#"C:\a "b""#);
        assert!(s.ends_with("\"Path\"=\"C:\\\\a \\\"b\\\"\"\r\n"), "{s}");
        assert_eq!(
            reg_value(&s, "Path"),
            Some((false, r#"C:\a "b""#.to_string()))
        );
    }

    #[test]
    fn a_notice_wraps_its_lines_under_its_title() {
        let lines = vec![
            "foldwave is uninstalled.".to_string(),
            "Your settings and pane logs are kept, so a later install finds them.".to_string(),
        ];
        let g = notice("UNINSTALL", &lines, 40, 12);
        let row = |r: usize| g[r].iter().map(|c| c.0).collect::<String>();
        assert!(row(0).starts_with("┌── [ FOLDWAVE ] · UNINSTALL "));
        assert!(row(2).contains("foldwave is uninstalled."));
        assert!(row(4).contains("Your settings and pane logs are"));
        assert!(row(5).contains("kept, so a later install finds"));
        assert!(row(10).contains("Enter or Esc closes"));
    }
}
