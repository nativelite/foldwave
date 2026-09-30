//! Build script, standard library only.
//!
//! - `fwterm.exe`'s own resources: the Reticle at every size Explorer asks
//!   for and the version Explorer and Task Manager show (`src/winres.rs`),
//!   drawn by the same code as the window's icon (`src/icon.rs`) and handed
//!   to the MSVC linker as a `.res` file.
//! - Microsoft's ConPTY files, carried inside the executable so an install
//!   can put them beside it: from `FOLDWAVE_CONPTY_DIR` when set
//!   (`tools/package.py` sets it), else empty and nothing is carried.

#[path = "src/icon.rs"]
#[allow(dead_code)]
mod icon;
#[path = "src/winres.rs"]
#[allow(dead_code)]
mod winres;

use std::env;
use std::fs;
use std::path::PathBuf;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=src/icon.rs");
    println!("cargo:rerun-if-changed=src/winres.rs");
    println!("cargo:rerun-if-env-changed=FOLDWAVE_CONPTY_DIR");
    let out = PathBuf::from(env::var_os("OUT_DIR").expect("OUT_DIR"));

    let conpty = env::var_os("FOLDWAVE_CONPTY_DIR").map(PathBuf::from);
    for name in ["conpty.dll", "OpenConsole.exe"] {
        let bytes = match &conpty {
            Some(dir) => {
                let path = dir.join(name);
                println!("cargo:rerun-if-changed={}", path.display());
                fs::read(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
            }
            None => Vec::new(),
        };
        fs::write(out.join(name), bytes).expect("write to OUT_DIR");
    }

    let windows_msvc = env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows")
        && env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc");
    if windows_msvc {
        let icons: Vec<(u32, Vec<u8>)> = icon::SIZES
            .iter()
            .copied()
            .chain([256])
            .map(|s| (s, icon::reticle(s)))
            .collect();
        let version = env::var("CARGO_PKG_VERSION").expect("CARGO_PKG_VERSION");
        let mut parts = version
            .split(['.', '-', '+'])
            .map(|p| p.parse::<u16>().unwrap_or(0));
        let numbers = (
            parts.next().unwrap_or(0),
            parts.next().unwrap_or(0),
            parts.next().unwrap_or(0),
        );
        let info = winres::version_info(
            numbers,
            &[
                ("CompanyName", "nativelite"),
                ("FileDescription", "foldwave"),
                ("FileVersion", &version),
                ("InternalName", "fwterm"),
                ("OriginalFilename", "fwterm.exe"),
                ("ProductName", "foldwave"),
                ("ProductVersion", &version),
            ],
        );
        let res = out.join("fwterm.res");
        fs::write(&res, winres::res_file(&icons, &info)).expect("write fwterm.res");
        println!("cargo:rustc-link-arg-bin=fwterm={}", res.display());
    }
}
