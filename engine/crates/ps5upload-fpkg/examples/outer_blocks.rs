//! Dump a package's outer image blocks — kind, head and tail — for comparing a package we
//! built against one the console accepts.
//!
//! `cargo run -p ps5upload-fpkg --example outer_blocks -- <pkg>`

use ps5upload_fpkg::{cnt, crypto, fih, outer, PkgFile};

fn main() -> ps5upload_fpkg::Result<()> {
    let path = std::env::args().nth(1).expect("usage: outer_blocks <pkg>");
    let mut pkg = PkgFile::open(std::path::Path::new(&path))?;
    let head = pkg.read_at(0, fih::HEADER_LEN)?;
    let parsed = fih::parse(&head)?;
    let container = cnt::read(&mut pkg, parsed.cnt_offset)?;
    let img = outer::open(&mut pkg, &parsed, &container, crypto::DEFAULT_PASSCODE)?;

    let hex = |b: &[u8]| {
        b.iter()
            .map(|x| format!("{x:02x}"))
            .collect::<Vec<_>>()
            .join(" ")
    };
    for (i, (block, verdict)) in img.plaintext.iter().zip(&img.verdicts).enumerate() {
        let nonzero = block.iter().filter(|b| **b != 0).count();
        println!(
            "blk {i:3} kind={:<12} nonzero={nonzero:6} head={} tail={}",
            format!("{:?}", verdict.kind),
            hex(&block[..16]),
            hex(&block[block.len() - 32..])
        );
    }
    Ok(())
}
