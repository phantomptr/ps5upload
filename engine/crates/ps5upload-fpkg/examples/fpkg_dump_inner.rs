//! Write a package's inner mount image out to a file, for offline inspection.
//!
//! `cargo run -p ps5upload-fpkg --example fpkg_dump_inner -- <pkg> <out>`

use ps5upload_fpkg::{cnt, crypto, fih, outer, PkgFile};

fn main() -> ps5upload_fpkg::Result<()> {
    let mut args = std::env::args().skip(1);
    let path = args.next().expect("usage: fpkg_dump_inner <pkg> <out>");
    let out = args.next().expect("usage: fpkg_dump_inner <pkg> <out>");

    let mut pkg = PkgFile::open(std::path::Path::new(&path))?;
    let head = pkg.read_at(0, fih::HEADER_LEN)?;
    let fih = fih::parse(&head)?;
    let container = cnt::read(&mut pkg, fih.cnt_offset)?;
    let img = outer::open(&mut pkg, &fih, &container, crypto::DEFAULT_PASSCODE)?;
    let nodes = img.dinodes();
    let image = img.file_data(&nodes[3]);
    std::fs::write(&out, &image)?;
    println!(
        "{path}: inner image {} bytes -> {out}; FIH 0x50 = {}, 0x60 = {}, 0x90 = {:#x}, 0xa0 = {:#x}",
        image.len(),
        u32::from_le_bytes(head[0x50..0x54].try_into().unwrap()),
        u32::from_le_bytes(head[0x60..0x64].try_into().unwrap()),
        u64::from_le_bytes(head[0x90..0x98].try_into().unwrap()),
        u64::from_le_bytes(head[0xa0..0xa8].try_into().unwrap()),
    );
    Ok(())
}
