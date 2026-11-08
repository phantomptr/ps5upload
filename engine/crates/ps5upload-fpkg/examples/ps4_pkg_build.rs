//! Write a PS4 fake package from a PS4 game (a folder, an image or a PS4 package), then read it
//! back and compare every file, as Convert does. For checking the writer against LibOrbisPkg's
//! `pkg_build` by hand.
//!
//! `cargo run -p ps5upload-fpkg --example ps4_pkg_build -- <game> <out.pkg> [unix-time]`

use std::sync::atomic::AtomicBool;

use ps5upload_fpkg::ps4_pkg_write::{read_back, write, Options};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 3 {
        eprintln!("usage: ps4_pkg_build <game> <out.pkg> [unix-time]");
        std::process::exit(2);
    }
    let time = args.get(3).map(|t| t.parse::<i64>().expect("a Unix time"));
    let mut tree = ps5upload_fpkg::source::open(std::path::Path::new(&args[1]))
        .unwrap_or_else(|e| panic!("{}: {e}", args[1]));
    let out_path = std::path::Path::new(&args[2]);
    let mut out = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .open(out_path)
        .unwrap_or_else(|e| panic!("{}: {e}", args[2]));
    let cancel = AtomicBool::new(false);
    let written = match write(
        tree.as_mut(),
        &mut out,
        &Options { time },
        &mut |_, _, _| {},
        &cancel,
    ) {
        Ok(w) => w,
        Err(e) => {
            eprintln!("refused: {e}");
            std::process::exit(1);
        }
    };
    drop(out);
    read_back(out_path, &written, &mut |_, _| {}, &cancel).expect("read back");
    println!(
        "{} {} bytes, {} files read back the same (licence {}, keystone {})",
        written.content_id,
        written.size,
        written.files.len(),
        if written.made_licence { "made" } else { "kept" },
        if written.made_keystone {
            "made"
        } else {
            "kept"
        },
    );
}
