fn main() {
    slint_build::compile("../app/ui/main.slint").unwrap();
    let out = std::path::PathBuf::from(std::env::var("OUT_DIR").unwrap()).join("mirror.rs");
    uimirror::generate(std::path::Path::new("../app/ui/state.slint"), &out);
    println!("cargo:rerun-if-changed=../app/ui/state.slint");
}
