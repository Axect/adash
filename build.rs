//! `include_dir!` embeds `web/` but does not list its files in the dep-info, so a change under
//! `web/` alone left the binary "Fresh" and `cargo install` shipped the old UI. Cargo scans a
//! directory given to `rerun-if-changed` recursively (new and removed files included), and a
//! rerun of this script rebuilds the crate.
fn main() {
    println!("cargo:rerun-if-changed=web");
    println!("cargo:rerun-if-changed=build.rs");
}
