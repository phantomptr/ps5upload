//! Tiny CLI to dump parsed PKG metadata. Useful for diagnostics:
//!   cargo run -p ps5upload-pkg --example probe -- /path/to/file.pkg

fn main() {
    let path = std::env::args()
        .nth(1)
        .expect("usage: probe <path/to/file.pkg>");
    match ps5upload_pkg::parse_pkg(std::path::Path::new(&path)) {
        Ok(m) => println!("{}", serde_json::to_string_pretty(&m).unwrap()),
        Err(e) => {
            eprintln!("ERR: {e}");
            std::process::exit(1);
        }
    }
}
