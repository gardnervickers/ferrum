fn main() {
    slint_build::compile("ui/main.slint").unwrap();
    let out = std::path::PathBuf::from(std::env::var("OUT_DIR").unwrap()).join("mirror.rs");
    uimirror::generate(std::path::Path::new("ui/state.slint"), &out);
    println!("cargo:rerun-if-changed=ui/state.slint");
    println!("cargo:rerun-if-env-changed=FERRUM_VERSION");
}
