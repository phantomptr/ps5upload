//! cargo run -p ps5upload-fpkg --example fpkg_verify -- <package> [passcode]

use ps5upload_fpkg::crypto::DEFAULT_PASSCODE;

fn main() {
    let mut args = std::env::args().skip(1);
    let Some(path) = args.next() else {
        eprintln!("usage: fpkg_verify <package> [passcode]");
        std::process::exit(2);
    };
    let passcode = args.next().unwrap_or_else(|| DEFAULT_PASSCODE.to_string());
    match ps5upload_fpkg::verify::verify_package(std::path::Path::new(&path), &passcode) {
        Ok(report) => {
            print!("{report}");
            std::process::exit(if report.ok() { 0 } else { 1 });
        }
        Err(e) => {
            eprintln!("error: {e}");
            std::process::exit(2);
        }
    }
}
