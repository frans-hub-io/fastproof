use std::path::Path;

fn main() {
    let dist = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../ui/dist");
    if dist.is_dir() {
        println!("cargo::rerun-if-changed={}", dist.display());
    }
    println!("cargo::rerun-if-changed=build.rs");
}
