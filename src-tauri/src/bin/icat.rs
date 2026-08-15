//! `icat` — the console entry point for Image Categorizer's extracted-text layer.
//!
//! Deliberately a console binary, not a windowed one: it is meant to be called by agents and from a
//! terminal, so stdout has to be a real pipe. The GUI keeps `windows_subsystem = "windows"`; this
//! one must not, or every invocation would return without printing anything.
//!
//! All the work lives in the library so the panel and the CLI share one implementation of scope,
//! freshness and ranking.

fn main() {
    std::process::exit(image_categorizer_tauri_lib::text_cli::run());
}
