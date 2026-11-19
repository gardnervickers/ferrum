fn main() {
    // The WASI build (the browser demo's engine) has no browser to decode
    // images in, though Slint treats every wasm32 target as a web page:
    // give it its images already decoded.
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("wasi") {
        let config = slint_build::CompilerConfiguration::new().embed_resources(slint_build::EmbedResourcesKind::EmbedForSoftwareRenderer);
        slint_build::compile_with_config("ui/main.slint", config).unwrap();
    } else {
        slint_build::compile("ui/main.slint").unwrap();
    }
    let out = std::path::PathBuf::from(std::env::var("OUT_DIR").unwrap()).join("mirror.rs");
    uimirror::generate(std::path::Path::new("ui/state.slint"), &out);
    println!("cargo:rerun-if-changed=ui/state.slint");
    println!("cargo:rerun-if-env-changed=FERRUM_VERSION");
}
