//! Decrypt and dump a package's SI `naps_meta_18.dat` TLV stream.
//!
//! `cargo run -p ps5upload-fpkg --example naps_dump -- <blob.bin>`
//!
//! The blob comes out of the install segment's ZIP (
//! `common/etc/naps_meta_18.dat`), encrypted with the fixed AES-128-XTS key set.

use ps5upload_fpkg::{crypto::XtsKeys, keys, xts::Xts};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = std::env::args().nth(1).expect("usage: naps_dump <blob>");
    let mut blob: Vec<u8> = std::fs::read(&path)?;
    Xts::new(&XtsKeys {
        tweak: keys::NAPS_META_18_TWEAK_KEY,
        data: keys::NAPS_META_18_DATA_KEY,
    })
    .decrypt(keys::NAPS_META_18_TWEAK_SECTOR, &mut blob);
    println!("{path}: naps_meta_18 {} bytes (decrypted)", blob.len());

    let mut at = 0usize;
    while at + 16 <= blob.len() {
        let tag: Vec<u8> = blob[at..at + 4].iter().rev().copied().collect();
        let len = u64::from_le_bytes(blob[at + 8..at + 16].try_into()?) as usize;
        let tag_s = String::from_utf8_lossy(&tag).to_string();
        println!("  @{at:#06x} tag={tag_s:6} len={len}",);
        if len > blob.len() {
            println!("     (truncated)");
            break;
        }
        let payload = &blob[at + 16..at + 16 + len];
        match tag_s.as_str() {
            "phdr" => {
                let words: Vec<u32> = payload
                    .chunks(4)
                    .map(|c| u32::from_le_bytes(c.try_into().unwrap_or([0; 4])))
                    .collect();
                println!("     phdr words: {words:?}");
            }
            "file" => {
                for e in payload.chunks(0x18).take(24) {
                    let size = u64::from_le_bytes(e[0..8].try_into()?);
                    let a = u32::from_le_bytes(e[8..12].try_into()?);
                    let b = u32::from_le_bytes(e[12..16].try_into()?);
                    let c = u32::from_le_bytes(e[16..20].try_into()?);
                    let d = u32::from_le_bytes(e[20..24].try_into()?);
                    println!("       file size={size} {a} {b} {c} {d}");
                }
            }
            "i2ob" => {
                for e in payload.chunks(0x28).take(40) {
                    let co = u64::from_le_bytes(e[0..8].try_into()?);
                    let cs = u32::from_le_bytes(e[8..12].try_into()?);
                    let ps = u32::from_le_bytes(e[12..16].try_into()?);
                    let c0 = u32::from_le_bytes(e[16..20].try_into()?);
                    let c1 = u32::from_le_bytes(e[20..24].try_into()?);
                    let x = u32::from_le_bytes(e[24..28].try_into()?);
                    let y = u32::from_le_bytes(e[28..32].try_into()?);
                    let z = u32::from_le_bytes(e[32..36].try_into()?);
                    let flag = u32::from_le_bytes(e[36..40].try_into()?);
                    println!(
                        "       co={co:#010x} cs={cs:#010x} ps={ps:#010x} c0={c0:#x} c1={c1:#x} x={x:#x} y={y} z={z} flag={flag:#010x}"
                    );
                }
            }
            "i2op" => {
                for e in payload.chunks(0x10).take(40) {
                    let co = u64::from_le_bytes(e[0..8].try_into()?);
                    let hi = u32::from_le_bytes(e[8..12].try_into()?);
                    println!("       i2op co={co:#010x} hi={hi:#x}");
                }
            }
            _ => {
                let head = &payload[..payload.len().min(48)];
                println!(
                    "     bytes: {}",
                    head.iter().map(|b| format!("{b:02x}")).collect::<String>()
                );
            }
        }
        at += 16 + len;
        if at > 1 << 20 {
            break;
        }
    }

    Ok(())
}
