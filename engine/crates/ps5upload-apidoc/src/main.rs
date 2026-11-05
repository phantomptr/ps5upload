//! Writes `engine/crates/ps5upload-engine/api/openapi.json` from the engine's source.
//!
//!     cargo run -p ps5upload-apidoc          # regenerate
//!     cargo run -p ps5upload-apidoc -- --check   # fail when the file is out of date
//!
//! Run from `engine/`, or anywhere: paths come from this crate's own location.

mod analyze;
mod routes;
mod schema;
mod source;
mod spec;

#[cfg(test)]
mod tests;

use std::path::{Path, PathBuf};

fn crates_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("this crate sits in crates/")
        .to_path_buf()
}

fn main() {
    let check = std::env::args().any(|a| a == "--check");
    let stats = std::env::args().any(|a| a == "--stats");
    let crates = crates_dir();
    let generated = match spec::generate(&crates) {
        Ok(generated) => generated,
        Err(problem) => {
            eprintln!("ps5upload-apidoc: {problem}");
            std::process::exit(2);
        }
    };
    if stats {
        eprintln!("{}", generated.stats);
    }
    let target = spec::output_path(&crates);
    if check {
        let current = std::fs::read_to_string(&target).unwrap_or_default();
        if let Some(problem) = spec::difference(&current, &generated.text) {
            eprintln!("{problem}");
            std::process::exit(1);
        }
        return;
    }
    if let Err(e) = std::fs::write(&target, &generated.text) {
        eprintln!("ps5upload-apidoc: cannot write {}: {e}", target.display());
        std::process::exit(2);
    }
    println!("wrote {}", target.display());
}
