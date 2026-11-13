//! Builds the NML shim when targeting the Pi. Other targets (the Mac) get only
//! the simulated machine. The shim must use the Pi's GNU toolchain pieces
//! (glibc, libstdc++) from MILLGUI_SYSROOT; see scripts/build.
use std::env;
use std::path::PathBuf;

fn main() {
    println!("cargo::rustc-check-cfg=cfg(has_nml)");
    println!("cargo:rerun-if-changed=cpp/lcnc_shim.cc");
    println!("cargo:rerun-if-changed=cpp/lcnc_shim.h");
    println!("cargo:rerun-if-env-changed=MILLGUI_SYSROOT");
    let target_os = env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let Some(sysroot) = env::var_os("MILLGUI_SYSROOT").map(PathBuf::from) else {
        if target_os == "linux" {
            println!("cargo:warning=MILLGUI_SYSROOT not set: building without the NML backend");
        }
        return;
    };
    if target_os != "linux" {
        return;
    }
    cc::Build::new()
        .cpp(true)
        .std("c++17")
        .file("cpp/lcnc_shim.cc")
        .include("cpp")
        .include(sysroot.join("usr/include/linuxcnc"))
        .warnings(false)
        .compile("lcnc_shim");
    println!("cargo:rustc-link-search=native={}", sysroot.join("usr/lib").display());
    println!("cargo:rustc-link-lib=static=linuxcnc");
    println!("cargo:rustc-link-lib=dylib=nml");
    println!("cargo:rustc-link-lib=dylib=linuxcncini");
    println!("cargo:rustc-link-lib=dylib=stdc++");
    println!("cargo:rustc-cfg=has_nml");
}
