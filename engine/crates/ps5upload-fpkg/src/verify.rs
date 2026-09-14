//! Every integrity check we understand, as a named list.

use std::fmt;
use std::path::Path;

use crate::cnt::{self, EntryDigest};
use crate::crypto::sha3;
use crate::outer::{self, BlockKind};
use crate::{fih, flt, le32, si, PkgFile, Result};

#[derive(Debug)]
pub struct Check {
    pub name: String,
    pub ok: bool,
    pub detail: String,
}

#[derive(Debug)]
pub struct Report {
    pub content_id: String,
    pub checks: Vec<Check>,
}

impl Report {
    pub fn ok(&self) -> bool {
        self.checks.iter().all(|c| c.ok)
    }

    fn push(&mut self, name: impl Into<String>, ok: bool, detail: impl Into<String>) {
        self.checks.push(Check {
            name: name.into(),
            ok,
            detail: detail.into(),
        });
    }
}

impl fmt::Display for Report {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "{}", self.content_id)?;
        for c in &self.checks {
            let mark = if c.ok { "ok  " } else { "FAIL" };
            writeln!(f, "  [{mark}] {} {}", c.name, c.detail)?;
        }
        Ok(())
    }
}

/// A dinode's file bytes, gathered from the plaintext blocks it points at.
fn file_data(img: &outer::OuterImage, node: Option<&outer::Dinode>) -> Vec<u8> {
    node.map(|n| img.file_data(n)).unwrap_or_default()
}

pub fn verify_package(path: &Path, passcode: &str) -> Result<Report> {
    let mut file = PkgFile::open(path)?;
    let fih_block = file.read_at(0, crate::BLOCK as usize)?;
    let fih = fih::parse(&fih_block)?;
    let cnt = cnt::read(&mut file, fih.cnt_offset)?;
    let mut r = Report {
        content_id: cnt.content_id.clone(),
        checks: Vec::new(),
    };

    r.push(
        "fih debug image",
        fih.is_debug(),
        format!("signed byte {:#04x}", fih.signed_byte),
    );
    r.push(
        "fih format version 3",
        fih.format_version == 3,
        fih.format_version.to_string(),
    );
    r.push("cnt package digest", cnt.package_digest_ok(), "");
    r.push("cnt digest-table digest", cnt.digest_table_digest_ok(), "");
    r.push("cnt header rollup digest", cnt.header_rollup_ok(), "");
    r.push("cnt body digest", cnt.body_digest_ok(), "");
    r.push(
        "cnt finalized-image digest",
        cnt.fih_digest_ok(&fih_block),
        "",
    );
    r.push("cnt descriptor pairs", cnt.descriptor_ok(), "");
    r.push(
        "cnt image-key digest",
        cnt.entry_digest_at(cnt::ids::IMAGE_KEY, 0x520),
        "",
    );
    r.push(
        "cnt imagedigs digest",
        cnt.entry_digest_at(cnt::ids::IMAGE_DIGESTS, 0x540),
        "",
    );
    for (id, verdict) in cnt.entry_digests() {
        r.push(
            format!("cnt entry {id:#06x} digest"),
            verdict != EntryDigest::Mismatch,
            format!("{verdict:?}"),
        );
    }
    if let Some(e) = cnt.entry(cnt::ids::PARAM_JSON) {
        let param = sha3(cnt.payload(e));
        let general = cnt
            .entry(cnt::ids::GENERAL_DIGESTS)
            .map(|g| cnt.payload(g))
            .unwrap_or(&[]);
        r.push(
            "param.json digest in GeneralDigests",
            general.windows(32).any(|w| w == param),
            "",
        );
    }
    for (name, ok) in cnt.general_digests(&fih.game_digest) {
        r.push(name, ok, "");
    }

    let img = outer::open(&mut file, &fih, &cnt, passcode)?;
    r.push(
        "outer superblock ICV",
        img.superblock.icv_ok,
        format!("block {}", img.superblock.index),
    );
    for v in &img.verdicts {
        r.push(
            format!("outer block {} decrypts to its imagedigs entry", v.index),
            v.kind.is_some(),
            match v.kind {
                Some(BlockKind::Data) => "data sector",
                Some(BlockKind::Signed) => "signed sector",
                Some(BlockKind::Superblock) => "plaintext superblock",
                None => "no sector matched",
            },
        );
    }
    let table_ok = img
        .plaintext
        .get(img.superblock.inode_table_block as usize)
        .is_some_and(|b| sha3(b) == img.superblock.inode_table_digest);
    r.push("outer inode table digest", table_ok, "");
    let nodes = img.dinodes();
    for (ino, n) in nodes.iter().enumerate() {
        let direct_ok = n
            .direct
            .iter()
            .take((n.blocks as usize).min(outer::DIRECT_SLOTS))
            .all(|d| {
                img.plaintext
                    .get(d.block as usize)
                    .is_some_and(|b| sha3(b) == d.digest)
            });
        // The indirect tables, level by level: a parent records its children's digests,
        // so every level is checked against the one above it.
        let used = n.indirect.iter().take_while(|t| t.block != 0).count();
        r.push(
            format!("outer inode {ino} block signatures"),
            direct_ok && img.indirect_ok(n),
            format!("{} block(s), {used} indirect level(s)", n.blocks),
        );
    }
    let uroot: Vec<String> = nodes
        .get(2)
        .map(|n| img.dirents(n).into_iter().map(|d| d.name).collect())
        .unwrap_or_default();
    r.push(
        "outer uroot holds pfs_image.dat and naps_pkg_layout.dat",
        uroot.iter().any(|n| n == "pfs_image.dat")
            && uroot.iter().any(|n| n == "naps_pkg_layout.dat"),
        uroot.join(", "),
    );

    // The outer flat-path table (inode 1) hashes each uroot dirent name to its inode.
    {
        let flt = file_data(&img, nodes.get(1));
        let count = flt.get(0x2C..0x30).map(|b| le32(b, 0)).unwrap_or(0) as usize;
        let mut checked = 0usize;
        let mut ok = count > 0;
        for d in nodes.get(2).map(|n| img.dirents(n)).unwrap_or_default() {
            if d.name == "." || d.name == ".." {
                continue;
            }
            let want = flt::hash_path(&d.name);
            let found = (0..count).any(|i| {
                let e = 0x40 + i * 16;
                flt.get(e..e + 16).is_some_and(|rec| {
                    u64::from_le_bytes(rec[..8].try_into().unwrap()) == want
                        && (u64::from_le_bytes(rec[8..].try_into().unwrap()) & 0xFF_FFFF)
                            == d.ino as u64
                })
            });
            checked += 1;
            ok &= found;
        }
        r.push(
            "outer flat-path table hashes the uroot names",
            ok && checked > 0,
            format!("{checked} name(s), {count} table entries"),
        );
    }

    match si::read(&mut file)? {
        Some(s) => {
            let crc_name = format!("config/{}/playgo-chunk.crc", cnt.content_id);
            match s.members.iter().find(|m| m.name == crc_name) {
                Some(m) => {
                    let stored = file.read_at(m.offset, m.size as usize)?;
                    let expected = si::chunk_crc_table(&mut file, s.zip_start)?;
                    r.push(
                        "si playgo-chunk.crc",
                        stored == expected,
                        format!("{} bytes", m.size),
                    );
                }
                None => r.push("si playgo-chunk.crc", false, format!("{crc_name} missing")),
            }
        }
        None => r.push("si zip present", false, "no trailing STORED ZIP"),
    }
    Ok(r)
}
