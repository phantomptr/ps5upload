//! Compress a game image (`.exfat` / `.ffpkg`) into a `.ffpfsc` for ShadowMountPlus.
//!
//! cargo run --release -p ps5upload-fpkg --example ffpfsc_wrap -- <source> <output> \
//!     [--level 1-9] [--gain PERCENT] [--time UNIX_SECONDS] [--name INNER]

use ps5upload_fpkg::ffpfsc::{self, Control, WrapOptions};

fn main() {
    let mut args = std::env::args().skip(1);
    let (Some(source), Some(output)) = (args.next(), args.next()) else {
        eprintln!(
            "usage: ffpfsc_wrap <source> <output> [--level N] [--gain N] [--time T] [--name S]"
        );
        std::process::exit(2);
    };
    let mut o = WrapOptions::default();
    while let Some(flag) = args.next() {
        let v = args.next().unwrap_or_default();
        match flag.as_str() {
            "--level" => o.level = v.parse().expect("level"),
            "--gain" => o.min_block_gain = v.parse().expect("gain"),
            "--time" => o.time = Some(v.parse().expect("time")),
            "--name" => o.inner_name = Some(v),
            other => {
                eprintln!("unknown argument {other}");
                std::process::exit(2);
            }
        }
    }
    let started = std::time::Instant::now();
    let mut last = 0u64;
    let mut progress = |done: u64, total: u64| {
        if done - last >= 1 << 30 || done == total {
            eprintln!(
                "  {:.1} / {:.1} GiB",
                done as f64 / (1u64 << 30) as f64,
                total as f64 / (1u64 << 30) as f64
            );
            last = done;
        }
    };
    let mut control = Control {
        progress: Some(&mut progress),
        cancel: None,
    };
    match ffpfsc::wrap(source.as_ref(), output.as_ref(), &o, &mut control) {
        Ok(r) => {
            let secs = started.elapsed().as_secs_f64();
            println!(
                "{} ({}): {} -> {} bytes ({:.1}% smaller), {}/{} blocks compressed, {:.0} MB/s, verified",
                r.output.display(),
                r.inner_name,
                r.raw_size,
                r.image_size,
                100.0 * (1.0 - r.image_size as f64 / r.raw_size as f64),
                r.compressed_blocks,
                r.blocks,
                r.raw_size as f64 / secs / 1e6
            );
        }
        Err(e) => {
            eprintln!("error: {e}");
            std::process::exit(1);
        }
    }
}
