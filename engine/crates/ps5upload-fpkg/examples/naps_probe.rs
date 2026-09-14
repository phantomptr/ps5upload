//! Print a package's `naps_pkg_layout.dat` decoded, with the header fields, the tail of the
//! `fidx` section and every record the last few ublocks map to.
//!
//! `cargo run -p ps5upload-fpkg --example naps_probe -- <pkg> [ublocks-from-the-end]`

use ps5upload_fpkg::{cnt, crypto, fih, naps, outer, PkgFile};

fn main() -> ps5upload_fpkg::Result<()> {
    let mut args = std::env::args().skip(1);
    let path = args.next().expect("usage: naps_probe <pkg> [tail]");
    let tail: usize = args.next().and_then(|v| v.parse().ok()).unwrap_or(6);

    let mut pkg = PkgFile::open(std::path::Path::new(&path))?;
    let head = pkg.read_at(0, fih::HEADER_LEN)?;
    let parsed = fih::parse(&head)?;
    let container = cnt::read(&mut pkg, parsed.cnt_offset)?;
    let image = outer::open(&mut pkg, &parsed, &container, crypto::DEFAULT_PASSCODE)?;
    let nodes = image.dinodes();
    let uroot = nodes.get(2).unwrap();
    let mut blob = None;
    for d in image.dirents(uroot) {
        if d.name == "naps_pkg_layout.dat" {
            blob = Some(image.file_data(&nodes[d.ino as usize]));
        }
    }
    let blob = blob.expect("no naps_pkg_layout.dat in uroot");
    if std::env::var("NAPS_HEX").is_ok() {
        // The section strides are validated by subtraction, which hides an off-by-N inside a
        // wrong stride; small descriptors are small enough to read directly.
        for (i, row) in blob.chunks(16).enumerate() {
            let cells: Vec<String> = row.iter().map(|b| format!("{b:02x}")).collect();
            println!("  {:04x}  {}", i * 16, cells.join(" "));
        }
        return Ok(());
    }
    for (i, n) in nodes.iter().enumerate() {
        println!(
            "  dinode {i}: flags={:#x} size={} size_compressed={} blocks={}",
            n.flags, n.size, n.size_compressed, n.blocks
        );
    }
    if std::env::var("NAPS_DINODES").is_ok() {
        return Ok(());
    }
    let stored = image.file_data(&nodes[3]);
    println!(
        "  FIH 0x50={} 0x60={:#x} 0x90={} 0x94={} 0xA0={} 0xA8={}",
        u32::from_le_bytes(head[0x50..0x54].try_into().unwrap()),
        u64::from_le_bytes(head[0x60..0x68].try_into().unwrap()),
        u32::from_le_bytes(head[0x90..0x94].try_into().unwrap()),
        u32::from_le_bytes(head[0x94..0x98].try_into().unwrap()),
        u64::from_le_bytes(head[0xA0..0xA8].try_into().unwrap()),
        u64::from_le_bytes(head[0xA8..0xB0].try_into().unwrap())
    );
    let word0 = u64::from_le_bytes(blob[0..8].try_into().unwrap());
    let word1 = u64::from_le_bytes(blob[8..16].try_into().unwrap());
    let num_ublocks = ((word0 >> 32) & 0xFF_FFFF) as u32;
    let num_outer = (word1 & 0xFF_FFFF) as u32;
    println!(
        "  raw header: word0={word0:#018x} word1={word1:#018x} \
         (files={}, comp={}, keys={}, shuffle={}, ublocks={num_ublocks}, outer={num_outer}, cblock={})",
        (word0 & 0xFF_FFFF) + 1,
        (word0 >> 24) & 0x3,
        ((word0 >> 26) & 0x3) + 1,
        (word0 >> 28) & 0xF,
        ((word1 >> 24) & 0xFF_FFFF) + 2
    );
    let layout = match naps::parse(&blob) {
        Ok(l) => l,
        Err(e) => {
            println!("  parse failed: {e} (blob is {} bytes)", blob.len());
            return Ok(());
        }
    };

    println!("{path}");
    println!(
        "  blob {} bytes; stored image {} bytes = {} ublocks",
        blob.len(),
        stored.len(),
        stored.len() as u64 / naps::UBLOCK
    );
    println!(
        "  num_files={} compType={} num_keys={} num_shuffle={}",
        layout.num_files, layout.compression_type, layout.num_keys, layout.num_shuffle
    );
    println!(
        "  num_ublocks={} num_outer_blocks={} cblocks={} u2c={}",
        layout.num_ublocks,
        layout.num_outer_blocks,
        layout.cblocks.len(),
        layout.u2c.len()
    );
    println!("  mount_size={:#x}", layout.mount_size());

    let n = layout.fidx.len();
    println!("  fidx ({n} entries), tail:");
    for (i, (offset, kind)) in layout.fidx.iter().enumerate().skip(n.saturating_sub(tail)) {
        println!("    [{i:6}] {offset:#012x} type {kind:#04x}");
    }

    // ublock -> cblockinfo index, through the u2c groups.
    let mut starts = Vec::with_capacity(layout.num_ublocks as usize);
    for (base, deltas) in &layout.u2c {
        starts.push(*base);
        for d in deltas {
            starts.push(*base + u32::from(*d));
        }
    }

    let mut runs: Vec<(usize, u64)> = Vec::new();
    for (i, c) in layout.cblocks.iter().enumerate() {
        if let naps::Cblock::RunBase {
            coffset_start_256k,
            tweak,
            ..
        } = c
        {
            runs.push((i, u64::from(*coffset_start_256k / 2) * naps::UBLOCK));
            let _ = tweak;
        }
    }
    println!("  {} run bases, last {}:", runs.len(), tail.min(runs.len()));
    for (i, on_disk) in runs.iter().rev().take(tail).rev() {
        println!(
            "    record {i:6} -> on-disk {on_disk:#012x} (ublock {})",
            on_disk / naps::UBLOCK
        );
    }

    println!("  last {} ublocks:", tail);
    for k in (0..layout.num_ublocks as usize).rev().take(tail).rev() {
        let uoffset = k as u64 * naps::UBLOCK;
        let index = starts.get(k).copied().unwrap_or(u32::MAX);
        match layout.cblocks.get(index as usize) {
            Some(naps::Cblock::Block {
                coffset_start_mod_256k,
                uoffset_start,
                clen_even_minus1,
                even,
                odd,
                kde,
                shuffle,
            }) => println!(
                "    ublock {k:5} @{uoffset:#012x} rec {index:6} coff={coffset_start_mod_256k:#08x} \
                 uoff={uoffset_start:#08x} clen={clen_even_minus1:#x} even={even} odd={odd} \
                 kde={kde} shuf={shuffle}"
            ),
            other => println!("    ublock {k:5} @{uoffset:#012x} rec {index:6} {other:?}"),
        }
    }

    if std::env::var("NAPS_ALL").is_ok() {
        println!("  every cblockinfo record:");
        for (i, c) in layout.cblocks.iter().enumerate() {
            match c {
                naps::Cblock::RunBase {
                    coffset_end_mod_256k,
                    tweak,
                    key_slot,
                    coffset_start_256k,
                } => println!(
                    "    [{i:5}] RUNBASE end_mod={coffset_end_mod_256k:#08x} tweak={tweak:#010x} \
                     slot={key_slot} start_256k={coffset_start_256k:#x} (-> ublock {})",
                    coffset_start_256k / 2
                ),
                naps::Cblock::Block {
                    coffset_start_mod_256k,
                    uoffset_start,
                    clen_even_minus1,
                    even,
                    odd,
                    kde,
                    shuffle,
                } => println!(
                    "    [{i:5}] coff={coffset_start_mod_256k:#08x} uoff={uoffset_start:#08x} \
                     clen={clen_even_minus1:#x} even={even} odd={odd} kde={kde} shuf={shuffle}"
                ),
            }
        }
    }
    Ok(())
}
