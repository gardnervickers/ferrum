//! Compiles the NML shim (feature `nml`) against LinuxCNC's headers.
//!
//! Where they are looked for, first match wins:
//! - `LCNC_INCLUDE_DIR` and `LCNC_LIB_DIR`;
//! - `LCNC_SYSROOT`: `<sysroot>/usr/include/linuxcnc` and `<sysroot>/usr/lib`
//!   (a cross build against the target's own glibc and libstdc++);
//! - `/usr/include/linuxcnc` (the `linuxcnc-uspace-dev` package).
//!
//! When none is found (or the target isn't Linux) the crate builds without
//! the backend and `Connection::open` returns `Error::NoBackend`.
use std::env;
use std::path::PathBuf;

fn main() {
    println!("cargo::rustc-check-cfg=cfg(lcnc_nml)");
    println!("cargo:rerun-if-changed=cpp/lcnc_shim.cc");
    println!("cargo:rerun-if-changed=cpp/lcnc_shim.h");
    for v in ["LCNC_INCLUDE_DIR", "LCNC_LIB_DIR", "LCNC_SYSROOT"] {
        println!("cargo:rerun-if-env-changed={v}");
    }
    if env::var_os("CARGO_FEATURE_NML").is_none() {
        return;
    }
    #[cfg(feature = "nml")]
    nml();
}

#[cfg(feature = "nml")]
fn nml() {
    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("linux") {
        return;
    }
    let var = |k: &str| env::var_os(k).map(PathBuf::from);
    let dirs = match (var("LCNC_INCLUDE_DIR"), var("LCNC_LIB_DIR"), var("LCNC_SYSROOT")) {
        (Some(inc), lib, _) => Some((inc, lib)),
        (None, _, Some(root)) => Some((root.join("usr/include/linuxcnc"), Some(root.join("usr/lib")))),
        _ => {
            let sys = PathBuf::from("/usr/include/linuxcnc");
            sys.join("emc_nml.hh").exists().then_some((sys, None))
        }
    };
    let Some((include, lib)) = dirs else {
        println!("cargo:warning=lcnc: LinuxCNC headers not found (set LCNC_SYSROOT or LCNC_INCLUDE_DIR): building without the NML backend");
        return;
    };
    cc::Build::new()
        .cpp(true)
        .std("c++17")
        .file("cpp/lcnc_shim.cc")
        .include("cpp")
        .include(&include)
        .warnings(false)
        .compile("lcnc_shim");
    if let Some(lib) = lib {
        println!("cargo:rustc-link-search=native={}", lib.display());
    }
    println!("cargo:rustc-link-lib=static=linuxcnc");
    println!("cargo:rustc-link-lib=dylib=nml");
    println!("cargo:rustc-link-lib=dylib=linuxcncini");
    println!("cargo:rustc-link-lib=dylib=stdc++");
    println!("cargo:rustc-cfg=lcnc_nml");
}
