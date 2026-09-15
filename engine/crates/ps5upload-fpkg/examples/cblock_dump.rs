//! Dump a package's `cblockinfo` records, located from the *end* of the descriptor.
//!
//! `naps_probe` needs the whole descriptor to parse, and the `fidx` stride is not known for
//! every real package (the reference's Minecraft build fails there). This section always ends
//! the blob and its length comes straight from the header word, so it can be read anyway —
//! which is what makes a side-by-side against a package that mounts possible.
//!
//! `cargo run -p ps5upload-fpkg --example cblock_dump -- <pkg> [records]`

use ps5upload_fpkg::naps::{self, Cblock, CBLOCK_LEN};
use ps5upload_fpkg::{cnt, crypto, fih, outer, PkgFile};

fn main() -> ps5upload_fpkg::Result<()> {
    let mut args = std::env::args().skip(1);
    let path = args.next().expect("usage: cblock_dump <pkg> [records]");
    let limit: usize = args.next().and_then(|v| v.parse().ok()).unwrap_or(12);

    let mut pkg = PkgFile::open(std::path::Path::new(&path))?;
    let head = pkg.read_at(0, fih::HEADER_LEN)?;
    let parsed = fih::parse(&head)?;
    let container = cnt::read(&mut pkg, parsed.cnt_offset)?;
    let image = outer::open(&mut pkg, &parsed, &container, crypto::DEFAULT_PASSCODE)?;
    let nodes = image.dinodes();
    let uroot = nodes.get(2).expect("no uroot dinode");
    let mut blob = None;
    for d in image.dirents(uroot) {
        if d.name == "naps_pkg_layout.dat" {
            blob = Some(image.file_data(&nodes[d.ino as usize]));
        }
    }
    let blob = blob.expect("no naps_pkg_layout.dat in uroot");

    let word0 = u64::from_le_bytes(blob[0..8].try_into().unwrap());
    let word1 = u64::from_le_bytes(blob[8..16].try_into().unwrap());
    let num_files = (word0 & 0xFF_FFFF) + 1;
    let num_ublocks = (word0 >> 32) & 0xFF_FFFF;
    let num_outer = word1 & 0xFF_FFFF;
    let num_cblock = ((word1 >> 24) & 0xFF_FFFF) + 2;

    println!("{path}");
    let comp = (word0 >> 24) & 0x3;
    let blob_len = blob.len();
    println!(
        "  blob {blob_len} bytes; files={num_files} comp={comp} ublocks={num_ublocks} outer={num_outer} cblocks={num_cblock}"
    );

    // The descriptor is [header][outer digests][shuffle patterns][fidx][u2c][cblockinfo], so
    // with only the header read the last section is found by counting back from the end.
    let at = blob
        .len()
        .checked_sub(num_cblock as usize * CBLOCK_LEN)
        .expect("the blob is shorter than its own record count");
    let digests_end = 16 + num_outer as usize * 8;
    let middle = at.saturating_sub(digests_end);
    println!(
        "  records start at {at:#x}; header+digests end at {digests_end:#x}; fidx+u2c occupy {middle:#x} bytes"
    );

    // The raw record bytes, for analysis outside this tool: a hex dump of a large descriptor
    // is unreadable and its offsets wrap at four digits.
    if let Ok(out) = std::env::var("CBLOCK_RAW") {
        std::fs::write(&out, &blob[at..])?;
        println!("  wrote {} record bytes to {out}", blob.len() - at);
    }
    if let Ok(out) = std::env::var("CBLOCK_BLOB") {
        std::fs::write(&out, &blob)?;
        println!("  wrote the whole {blob_len}-byte descriptor to {out} (records start {at:#x})");
    }

    let mut run_bases: Vec<usize> = Vec::new();
    let mut blocks: Vec<usize> = Vec::new();
    for i in 0..num_cblock as usize {
        let raw: &[u8; CBLOCK_LEN] = blob[at + i * CBLOCK_LEN..at + (i + 1) * CBLOCK_LEN]
            .try_into()
            .unwrap();
        match naps::decode_cblock(raw) {
            Cblock::RunBase { .. } => run_bases.push(i),
            Cblock::Block { .. } => blocks.push(i),
        }
    }
    println!(
        "  {} run bases, {} block records",
        run_bases.len(),
        blocks.len()
    );
    if run_bases.len() > 1 {
        let gaps: Vec<usize> = run_bases.windows(2).map(|w| w[1] - w[0]).collect();
        let mut sorted = gaps.clone();
        sorted.sort_unstable();
        sorted.dedup();
        println!(
            "  run-base gaps: {} distinct, first {:?}",
            sorted.len(),
            &gaps[..gaps.len().min(12)]
        );
    }

    println!("  first {limit} records:");
    for i in 0..num_cblock as usize {
        let raw: &[u8; CBLOCK_LEN] = blob[at + i * CBLOCK_LEN..at + (i + 1) * CBLOCK_LEN]
            .try_into()
            .unwrap();
        match naps::decode_cblock(raw) {
            Cblock::RunBase {
                coffset_end_mod_256k,
                tweak,
                key_slot,
                coffset_start_256k,
            } => println!(
                "    [{i:6}] RUNBASE end={coffset_end_mod_256k:#08x} tweak={tweak:#010x} \
                 slot={key_slot} start_256k={coffset_start_256k:#x}"
            ),
            Cblock::Block {
                coffset_start_mod_256k,
                reserved19: _,
                uoffset_start,
                clen_even_minus1,
                even,
                odd,
                kde,
                shuffle,
            } => println!(
                "    [{i:6}] coff={coffset_start_mod_256k:#08x} uoff={uoffset_start:#08x} \
                 clen={clen_even_minus1:#x} even={even} odd={odd} kde={kde} shuf={shuffle}"
            ),
        }
        if i + 1 >= limit {
            break;
        }
    }
    Ok(())
}
