//! Every integrity check we understand, as a named list.

use std::fmt;
use std::path::Path;

use crate::cnt::{self, EntryDigest};
use crate::crypto::sha3;
use crate::outer::{self, BlockKind};
use crate::{fih, si, PkgFile, Result};

pub struct Check {
    pub name: String,
    pub ok: bool,
    pub detail: String,
}

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

pub fn verify_package(path: &Path, passcode: &str) -> Result<Report> {
    let mut file = PkgFile::open(path)?;
    let head = file.read_at(0, fih::HEADER_LEN)?;
    let fih = fih::parse(&head)?;
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
        let ok = n.direct.iter().take(n.blocks.min(12) as usize).all(|d| {
            img.plaintext
                .get(d.block as usize)
                .is_some_and(|b| sha3(b) == d.digest)
        });
        r.push(
            format!("outer inode {ino} block signatures"),
            ok,
            format!("{} block(s)", n.blocks),
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
