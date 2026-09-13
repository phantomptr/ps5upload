//! Build a debug FPKG from a game folder.
//!
//! cargo run -p ps5upload-fpkg --example fpkg_build -- <folder> <output-dir> [--content-id ID]

use ps5upload_fpkg::build::{self, BuildRequest};

fn main() {
    let mut args = std::env::args().skip(1);
    let (Some(source), Some(output_dir)) = (args.next(), args.next()) else {
        eprintln!("usage: fpkg_build <source-folder> <output-dir> [--content-id ID] [--name STEM]");
        std::process::exit(2);
    };
    let mut request = BuildRequest::new(source, output_dir);
    while let Some(flag) = args.next() {
        match flag.as_str() {
            "--content-id" => request.content_id = args.next(),
            "--name" => request.file_name = args.next(),
            other => {
                eprintln!("unknown argument {other}");
                std::process::exit(2);
            }
        }
    }
    let mut progress = |phase: &str| eprintln!("  {phase}...");
    match build::build(&request, &mut progress) {
        Ok(report) => {
            for warning in &report.warnings {
                eprintln!("  warning: {warning}");
            }
            print!("{}", report.verify);
            eprintln!("{}", build::summary(&report));
            std::process::exit(if report.verify.ok() { 0 } else { 1 });
        }
        Err(e) => {
            eprintln!("error: {e}");
            std::process::exit(2);
        }
    }
}
