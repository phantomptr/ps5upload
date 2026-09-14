//! The install manifest (`common/etc/pfsimage.xml`) the SI archive carries.
//!
//! The console reads it: a package built without the member is rejected at
//! `GetRawContentInfo` with an empty title id (`0x80120004`), while one carrying an
//! earlier, invented `package-config` document gets past that stage and into the
//! transfer. The element set, attribute names, ordering and number formats here are the
//! ones measured on the scene packages that install (`Crimson.Desert…-DUPLEX`,
//! `EP7579-PPSA17599…`, `…DONPACK…`), filled with this build's own geometry.

use crate::cnt_write::Facts;
use crate::outer_write::Layout;
use crate::plan::{
    self, Plan, AFID_TABLE_INODE, APR_FLT_INODE, DIRENT_DIR, DIRENT_DOT, DIRENT_DOTDOT,
    DIRENT_FILE, FLAGS_DATA, FLAGS_TABLE, INODE_FLT_INODE, MODE_DIR,
};
use crate::BLOCK;
use std::fmt::Write as _;

/// Everything the manifest reports, all of it already fixed by the time the container
/// exists. The two writers (streaming and in-memory) fill it from the same plan, so the
/// document they emit is byte-identical.
pub struct ManifestParams<'a> {
    pub facts: &'a Facts,
    pub content_id: &'a str,
    /// `0x21` for additional content, `0x26` for an app.
    pub content_type: u32,
    /// The packaged `param.json`, whose fields the manifest repeats.
    pub param_json: &'a [u8],
    /// The content version's 2-3-3 BCD word (`FIH 0x9C`).
    pub content_version: u32,
    /// Absolute offset of the container.
    pub cnt_offset: u64,
    /// Absolute offset of the install segment: the end of everything the CRC table covers.
    pub si_offset: u64,
    /// The encrypted outer image's length.
    pub outer_size: u64,
    /// The block-aligned inner image size.
    pub inner_size: u64,
    pub seed: [u8; 16],
    pub game_digest: [u8; 32],
    /// The outer superblock's ICV.
    pub icv: [u8; 32],
    /// The `playgo-chunk.dat` member's length.
    pub playgo_chunk_len: u64,
    /// The outer image's block map, which supplies every `index`.
    pub outer: &'a Layout,
    /// `naps_pkg_layout.dat`'s length.
    pub naps_len: u64,
    pub plan: &'a Plan,
}

/// A 32-byte digest as the samples render it: `0xNN` bytes, sixteen to a line, indented
/// six spaces, closing tag at four.
fn digest(xml: &mut String, indent: usize, tag: &str, value: &[u8; 32]) {
    let pad = " ".repeat(indent);
    let _ = writeln!(xml, "{pad}<{tag}>");
    for row in value.chunks(16) {
        let line: Vec<String> = row.iter().map(|b| format!("0x{b:02x}")).collect();
        let _ = writeln!(xml, "      {}", line.join(" "));
    }
    let _ = writeln!(xml, "{pad}</{tag}>");
}

/// A hex word the samples print bare, without separators, in upper case.
fn hex_upper(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = write!(out, "{b:02X}");
    }
    out
}

/// The `2-3-3` BCD content version as `MM.mmm.ppp`.
fn dotted_version(word: u32) -> String {
    format!(
        "{:02x}.{:03x}.{:03x}",
        (word >> 24) & 0xFF,
        (word >> 12) & 0xFFF,
        word & 0xFFF
    )
}

/// A firmware word as the manifest's `2-3-3-8` dotted form: the sixteen BCD digits of the
/// parameter, split 2/3/3/8. `0x1160000000000000` reads as `11.600.000.00000000`.
fn dotted_firmware(word: &str) -> String {
    let digits: String = word
        .trim_start_matches("0x")
        .trim_start_matches("0X")
        .chars()
        .filter(|c| c.is_ascii_hexdigit())
        .collect();
    if digits.len() != 16 {
        return "0.0.0.0".to_string();
    }
    format!(
        "{}.{}.{}.{}",
        &digits[0..2],
        &digits[2..5],
        &digits[5..8],
        &digits[8..16]
    )
}

/// XML text escaping for the values that come from `param.json`.
fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// A string field of the packaged `param.json`, if it is a string.
fn param_field(param_json: &[u8], key: &str) -> Option<String> {
    let text = String::from_utf8_lossy(param_json);
    let json: serde_json::Value = serde_json::from_str(text.trim_start_matches('\u{feff}')).ok()?;
    Some(json.get(key)?.as_str()?.to_string())
}

/// The inner tree's inode numbers: `dirs[0]` is uroot at inode 4, files follow the
/// directories in inode order.
fn inner_file_index(plan: &Plan, inode: u32) -> Option<usize> {
    let first = 4 + plan.dirs.len() as u32;
    (inode >= first).then(|| (inode - first) as usize)
}

/// One directory of the inner tree, its children in dirent order. `.` and `..` are the
/// mount's own links and are not listed.
fn inner_dir(xml: &mut String, p: &ManifestParams, index: usize, depth: usize) {
    let plan = p.plan;
    let dir = &plan.dirs[index];
    let (offset, bytes) = plan.metadata.dirs[index];
    let pad = " ".repeat(depth * 2);
    // The mount root's record carries the metadata table flag; every other directory is a
    // plain directory. Both match what `inner::write` puts in the inode table.
    let imode = if index == 0 { FLAGS_DATA } else { FLAGS_TABLE };
    let mode = if index == 0 {
        String::new()
    } else {
        format!(" mode=\"{MODE_DIR:#06x}\"")
    };
    // The mount root is `uroot`; the plan's own path for it is empty.
    let name = if index == 0 {
        "uroot".to_string()
    } else {
        dir.path.rsplit('/').next().unwrap_or(&dir.path).to_string()
    };
    let _ = writeln!(
        xml,
        "{pad}<dir plain=\"{bytes}\" poffset=\"{offset}\" links=\"{}\"{mode} imode=\"{imode:#010x}\" inode=\"{}\" name=\"{}\">",
        dir.nlink,
        dir.inode,
        escape(&name),
    );
    for (name, inode, kind) in &dir.dirents {
        if *kind == DIRENT_DOT || *kind == DIRENT_DOTDOT {
            continue;
        }
        if *kind == DIRENT_DIR {
            inner_dir(xml, p, (*inode - 4) as usize, depth + 1);
        } else if *kind == DIRENT_FILE {
            if let Some(fi) = inner_file_index(plan, *inode) {
                let f = &plan.files[fi];
                let _ = writeln!(
                    xml,
                    "{pad}  <file plain=\"{}\" poffset=\"{}\" imode=\"{:#010x}\" inode=\"{}\" name=\"{}\"/>",
                    f.size,
                    f.logical_offset,
                    f.inode_flags(),
                    f.inode,
                    escape(name),
                );
            }
        }
    }
    let _ = writeln!(xml, "{pad}</dir>");
}

/// The document.
pub fn build(p: &ManifestParams) -> Vec<u8> {
    let mount = p.plan;
    let facts = p.facts;
    let add_on = p.content_type == 0x21;
    let suffix = if add_on { "AC" } else { "GD" };
    let content_type = format!("PS5{suffix}");
    let content_version = dotted_version(p.content_version);
    let title = param_field(p.param_json, "titleName").unwrap_or_default();
    let master = param_field(p.param_json, "masterVersion").unwrap_or_else(|| "01.00".to_string());
    let firmware = param_field(p.param_json, "requiredSystemSoftwareVersion")
        .unwrap_or_else(|| "0x0100000000000000".to_string());
    // The container's own block indices: the data starts at the image's first block, and
    // every metadata structure keeps its `Layout` index.
    let flt_len = crate::flt::write(&mount.flt_inode).len() as u64;
    let meta_region = mount.metadata.blocks * BLOCK;
    let blocks = |bytes: u64| bytes.div_ceil(BLOCK).max(1);

    let mut xml = String::with_capacity(4096);
    xml.push_str("<?xml version=\"1.0\" encoding=\"utf-8\"?>\n");
    xml.push_str("<package-configuration version=\"1.0\" type=\"package-info\">\n");
    let _ = writeln!(
        xml,
        "  <config version=\"{content_version}\" metadata=\"0\" primary=\"yes\">"
    );
    let _ = writeln!(xml, "    <content-id>{}</content-id>", escape(p.content_id));
    let _ = writeln!(xml, "    <primary-id>{}</primary-id>", escape(p.content_id));
    let _ = writeln!(
        xml,
        "    <longname>{}-C00000000-M0000000000000000-{suffix}</longname>",
        escape(p.content_id)
    );
    let _ = writeln!(
        xml,
        "    <required-system-version>{}</required-system-version>",
        dotted_firmware(&firmware)
    );
    xml.push_str("    <drm-type version=\"1\">PS5</drm-type>\n");
    let _ = writeln!(xml, "    <content-type>{content_type}</content-type>");
    xml.push_str("    <num-of-images>1</num-of-images>\n");
    let _ = writeln!(xml, "    <package-size>{}</package-size>", p.si_offset);
    xml.push_str("    <version-date>0x20240508</version-date>\n");
    xml.push_str("    <version-hash>0x090fbfc1</version-hash>\n");
    xml.push_str("  </config>\n");

    xml.push_str("  <digests version=\"1.2\" major-param-version=\"0\">\n");
    digest(&mut xml, 4, "content-digest", &facts.content_digest);
    digest(&mut xml, 4, "game-digest", &p.game_digest);
    digest(&mut xml, 4, "header-digest", &facts.header_digest);
    digest(&mut xml, 4, "system-digest", &facts.system_digest);
    digest(&mut xml, 4, "param-digest", &facts.param_digest);
    digest(&mut xml, 4, "package-digest", &facts.package_digest);
    xml.push_str("  </digests>\n");

    xml.push_str("  <params>\n");
    let _ = writeln!(xml, "    <contentId>{}</contentId>", escape(p.content_id));
    let _ = writeln!(
        xml,
        "    <contentVersion>{content_version}</contentVersion>"
    );
    let _ = writeln!(
        xml,
        "    <masterVersion>{}</masterVersion>",
        escape(&master)
    );
    let _ = writeln!(
        xml,
        "    <requiredSystemSoftwareVersion>{}</requiredSystemSoftwareVersion>",
        escape(&firmware)
    );
    let _ = writeln!(xml, "    <titleName>{}</titleName>", escape(&title));
    xml.push_str("  </params>\n");

    xml.push_str("  <container nth-of-image=\"1\">\n");
    let _ = writeln!(
        xml,
        "    <container-size>0x{:016x}</container-size>",
        facts.container_size
    );
    let _ = writeln!(
        xml,
        "    <mandatory-size>0x{:016x}</mandatory-size>",
        facts.mandatory_size
    );
    let _ = writeln!(
        xml,
        "    <body-offset>0x{:016x}</body-offset>",
        facts.body_offset
    );
    let _ = writeln!(xml, "    <body-size>0x{:016x}</body-size>", facts.body_size);
    digest(&mut xml, 4, "body-digest", &facts.body_digest);
    xml.push_str("  </container>\n");

    xml.push_str("  <mount-image nth-of-image=\"1\" nested-image=\"yes\">\n");
    let _ = writeln!(
        xml,
        "    <pfs-offset-align>0x{:016x}</pfs-offset-align>",
        BLOCK
    );
    let _ = writeln!(xml, "    <pfs-size-align>0x{:016x}</pfs-size-align>", BLOCK);
    let _ = writeln!(
        xml,
        "    <pfs-image-offset>0x{:016x}</pfs-image-offset>",
        BLOCK
    );
    let _ = writeln!(
        xml,
        "    <pfs-image-size>0x{:016x}</pfs-image-size>",
        p.outer_size
    );
    let _ = writeln!(
        xml,
        "    <fixed-info-size>0x{:08x}</fixed-info-size>",
        BLOCK
    );
    xml.push_str("    <pfs-image-seed>\n");
    for row in p.seed.chunks(16) {
        let line: Vec<String> = row.iter().map(|b| format!("0x{b:02x}")).collect();
        let _ = writeln!(xml, "      {}", line.join(" "));
    }
    xml.push_str("    </pfs-image-seed>\n");
    digest(&mut xml, 4, "sblock-digest", &p.game_digest);
    digest(&mut xml, 4, "fixed-info-digest", &facts.fih_digest);
    xml.push_str("    <mount-image-offset>0x0000000000000000</mount-image-offset>\n");
    let _ = writeln!(
        xml,
        "    <mount-image-size>0x{:016x}</mount-image-size>",
        p.si_offset
    );
    let _ = writeln!(
        xml,
        "    <container-offset>0x{:016x}</container-offset>",
        p.cnt_offset
    );
    let _ = writeln!(
        xml,
        "    <supplemental-offset>0x{:016x}</supplemental-offset>",
        facts.container_size
    );
    xml.push_str("  </mount-image>\n");

    let _ = writeln!(
        xml,
        "  <entries nth-of-image=\"1\" num=\"{}\">",
        facts.entries.len()
    );
    for (offset, size, name) in &facts.entries {
        let _ = writeln!(
            xml,
            "    <entry offset=\"0x{offset:08x}\" size=\"0x{size:08x}\" name=\"{}\"/>",
            escape(name)
        );
    }
    xml.push_str("  </entries>\n");

    // The chunk map the container's own `playgo-chunk.dat` declares: one scenario, one
    // chunk and one mchunk spanning `[0, cnt_offset)`.
    let mchunk = p.cnt_offset;
    let scenario_type = p.content_type;
    let _ = writeln!(
        xml,
        "  <chunkinfo size=\"{}\" nested=\"true\" sdk=\"0x00850000\" disps=\"0x0011\">",
        p.playgo_chunk_len
    );
    let _ = writeln!(xml, "    <contentid>{}</contentid>", escape(p.content_id));
    xml.push_str("    <languages default=\"1\">0xffffffffffffffff</languages>\n");
    xml.push_str("    <scenarios num=\"1\" default=\"0\" groups=\"0\">\n");
    let _ = writeln!(
        xml,
        "      <scenario id=\"0\" type=\"{scenario_type}\" name=\"\">"
    );
    let _ = writeln!(
        xml,
        "        <overall initials=\"1\" num=\"1\" init-size=\"{mchunk}\" total=\"{mchunk}\">0</overall>"
    );
    let _ = writeln!(
        xml,
        "        <default initials=\"1\" num=\"1\" init-size=\"{mchunk}\" total=\"{mchunk}\">0</default>"
    );
    xml.push_str("      </scenario>\n");
    xml.push_str("    </scenarios>\n");
    xml.push_str("    <chunks num=\"1\" default=\"0xffffffffffffffff\">\n");
    let _ = writeln!(
        xml,
        "      <chunk id=\"0\" flag=\"0x80\" locus=\"0x03\" language=\"0xffffffffffffffff\" disps=\"0x0011\" num=\"1\" size=\"{mchunk}\" name=\"\">0</chunk>"
    );
    xml.push_str("    </chunks>\n");
    xml.push_str("    <outers num=\"1\" overlapped=\"0\" language-overlapped=\"0\">\n");
    let _ = writeln!(
        xml,
        "      <outer id=\"0\" image=\"0\" offset=\"0x0000000000000000\" size=\"0x{mchunk:016x}\" chunks=\"1\"/>"
    );
    xml.push_str("    </outers>\n");
    xml.push_str("  </chunkinfo>\n");

    // The outer PFS: the five-inode template this writer lays out, block for block.
    let outer = p.outer;
    let _ = writeln!(
        xml,
        "  <pfs-image version=\"2\" readonly=\"true\" offset=\"{}\" metadata=\"{}\">",
        BLOCK,
        (outer.uroot_block + 1) * BLOCK
    );
    xml.push_str(
        "    <sblock signed=\"true\" encrypted=\"true\" ignore-case=\"true\" index-size=\"32\" blocks=\"1\" backups=\"0\">\n",
    );
    let _ = writeln!(
        xml,
        "      <image-size block-size=\"{BLOCK}\" num=\"{}\">0x{:016x}</image-size>",
        p.outer_size / BLOCK,
        p.outer_size
    );
    xml.push_str("      <super-inode blocks=\"1\" inodes=\"5\" root=\"0\">\n");
    let _ = writeln!(
        xml,
        "        <inode size=\"{BLOCK}\" links=\"1\" mode=\"0x0000\" imode=\"0x00000000\" index=\"{}\"/>",
        outer.table_block
    );
    xml.push_str("      </super-inode>\n");
    let _ = writeln!(xml, "      <seed>0x{}</seed>", hex_upper(&p.seed));
    let _ = writeln!(xml, "      <icv>0x{}</icv>", hex_upper(&p.icv));
    xml.push_str("    </sblock>\n");
    let _ = writeln!(
        xml,
        "    <root size=\"{BLOCK}\" links=\"1\" imode=\"0x0002000c\" index=\"{}\" inode=\"0\" name=\"\">",
        outer.root_block
    );
    let _ = writeln!(
        xml,
        "      <file size=\"{flt_len}\" imode=\"0x0002000c\" index=\"{}\" inode=\"1\" name=\"inode_flat_path_table\"/>",
        outer.flt_block
    );
    let _ = writeln!(
        xml,
        "      <dir size=\"{BLOCK}\" links=\"3\" imode=\"0x0000000c\" index=\"{}\" inode=\"2\" name=\"uroot\">",
        outer.uroot_block
    );
    let inner_blocks = blocks(p.inner_size);
    let _ = writeln!(
        xml,
        "        <file size=\"{}\" plain=\"{}\" comp=\"0% ({inner_blocks}/{inner_blocks})\" imode=\"0x0000000d\" index=\"0\" inode=\"3\" name=\"pfs_image.dat\"/>",
        p.inner_size, p.inner_size
    );
    let _ = writeln!(
        xml,
        "        <file size=\"{}\" imode=\"0x0000000d\" index=\"{}\" inode=\"4\" name=\"naps_pkg_layout.dat\"/>",
        p.naps_len, outer.naps_block
    );
    xml.push_str("      </dir>\n");
    xml.push_str("    </root>\n");
    xml.push_str("  </pfs-image>\n");

    // The inner mount: its superblock block, the metadata region's extent, then the tree.
    let meta_blocks = blocks(meta_region);
    let _ = writeln!(
        xml,
        "  <nested-image version=\"2\" readonly=\"true\" offset=\"0\">"
    );
    xml.push_str(
        "    <sblock ignore-case=\"true\" index-size=\"32\" blocks=\"1\" backups=\"0\">\n",
    );
    let _ = writeln!(
        xml,
        "      <image-size block-size=\"{BLOCK}\" num=\"{}\">0x{:016x}</image-size>",
        blocks(p.cnt_offset),
        p.cnt_offset
    );
    let _ = writeln!(
        xml,
        "      <super-inode blocks=\"1\" inodes=\"{}\" root=\"0\">",
        4 + mount.dirs.len() + mount.files.len()
    );
    let _ = writeln!(
        xml,
        "        <inode size=\"{BLOCK}\" links=\"1\" mode=\"0x0000\" imode=\"{:#010x}\" index=\"{}\"/>",
        FLAGS_TABLE,
        mount.meta_base / BLOCK
    );
    xml.push_str("      </super-inode>\n");
    xml.push_str("    </sblock>\n");
    let _ = writeln!(
        xml,
        "    <metadata size=\"{meta_region}\" plain=\"{meta_region}\" comp=\"0% ({meta_blocks}/{meta_blocks})\" offset=\"{}\" poffset=\"{}\" afid=\"0\"/>",
        mount.meta_base, mount.meta_base
    );
    let (root_at, root_len) = mount.metadata.super_root;
    let _ = writeln!(
        xml,
        "    <root plain=\"{root_len}\" poffset=\"{root_at}\" links=\"1\" imode=\"{:#010x}\" inode=\"0\" name=\"\">",
        FLAGS_TABLE
    );
    for (name, inode, kind) in &plan::super_root_dirents() {
        let (offset, bytes) = match *inode {
            INODE_FLT_INODE => mount.metadata.flt,
            inode if inode == APR_FLT_INODE => mount.metadata.flt_apr,
            inode if inode == AFID_TABLE_INODE => mount.metadata.afid,
            _ => (0, 0),
        };
        if *kind == DIRENT_FILE {
            let _ = writeln!(
                xml,
                "      <file plain=\"{bytes}\" poffset=\"{offset}\" imode=\"{FLAGS_TABLE:#010x}\" inode=\"{inode}\" name=\"{}\"/>",
                escape(name)
            );
        }
    }
    // `uroot` and everything below it, in pre-order, exactly as the mount lays them out.
    inner_dir(&mut xml, p, 0, 3);
    xml.push_str("    </root>\n");
    xml.push_str("  </nested-image>\n");
    xml.push_str("</package-configuration>\n");
    xml.into_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A manifest for a small tree, built the way both writers build it.
    fn sample() -> String {
        use crate::source::SourceFile;
        let files: Vec<SourceFile> = [
            ("eboot.bin", 4000u64),
            ("data/one.bin", 600_000),
            ("sce_sys/param.json", 197),
            ("sce_sys/icon0.png", 2048),
            ("sce_sys/icon0.dds", 4096),
            ("sce_sys/about/right.sprx", 4),
        ]
        .iter()
        .map(|(p, s)| SourceFile {
            path: (*p).to_string(),
            size: *s,
        })
        .collect();
        let plan = crate::plan::build(&files).unwrap();
        let facts = Facts {
            container_size: 0x1_0000,
            mandatory_size: 0x3991,
            body_offset: 0x2000,
            body_size: 0xE000,
            body_digest: [2u8; 32],
            package_digest: [3u8; 32],
            fih_digest: [4u8; 32],
            content_digest: [5u8; 32],
            header_digest: [6u8; 32],
            system_digest: [7u8; 32],
            param_digest: [8u8; 32],
            entries: vec![
                (0x38f7, 0x9a, "param.json"),
                (0x3c31, 0x190, "playgo-chunk.dat"),
            ],
        };
        let outer = crate::outer_write::layout(plan.ndblock, 4096).unwrap();
        let params = ManifestParams {
            facts: &facts,
            content_id: "UP0000-PPSA01234_00-TESTGAME00000000",
            content_type: 0x26,
            param_json: br#"{"titleName":"Test & Co","masterVersion":"01.00","requiredSystemSoftwareVersion":"0x1160000000000000"}"#,
            content_version: 0x0100_0000,
            cnt_offset: 0x16_0000,
            si_offset: 0x17_0000,
            outer_size: 0x15_0000,
            inner_size: 0xE_0000,
            seed: [0x11; 16],
            game_digest: [0x22; 32],
            icv: [0x33; 32],
            playgo_chunk_len: 400,
            outer: &outer,
            naps_len: 247,
            plan: &plan,
        };
        String::from_utf8(build(&params)).unwrap()
    }

    /// The tag of a line that opens an element, if it is one.
    fn tag(line: &str) -> Option<String> {
        let line = line.trim_start();
        let rest = line.strip_prefix('<')?;
        let rest = rest.strip_prefix('/').unwrap_or(rest);
        if line.starts_with("</") {
            return None;
        }
        let end = rest.find(|c: char| c.is_whitespace() || c == '>' || c == '/')?;
        Some(rest[..end].to_string())
    }

    #[test]
    fn the_document_carries_the_sample_element_set() {
        let xml = sample();
        let tags: Vec<String> = xml.lines().filter_map(tag).collect();
        // The sections, in the order every sample writes them.
        let sections: Vec<String> = tags
            .iter()
            .filter(|t| {
                matches!(
                    t.as_str(),
                    "package-configuration"
                        | "config"
                        | "digests"
                        | "params"
                        | "container"
                        | "mount-image"
                        | "entries"
                        | "chunkinfo"
                        | "pfs-image"
                        | "nested-image"
                )
            })
            .cloned()
            .collect();
        assert_eq!(
            sections,
            [
                "package-configuration",
                "config",
                "digests",
                "params",
                "container",
                "mount-image",
                "entries",
                "chunkinfo",
                "pfs-image",
                "nested-image",
            ]
            .iter()
            .map(|s| s.to_string())
            .collect::<Vec<_>>()
        );
        for element in [
            "content-id",
            "primary-id",
            "longname",
            "required-system-version",
            "drm-type",
            "content-type",
            "num-of-images",
            "package-size",
            "version-date",
            "version-hash",
            "content-digest",
            "game-digest",
            "header-digest",
            "system-digest",
            "param-digest",
            "package-digest",
            "contentId",
            "contentVersion",
            "masterVersion",
            "requiredSystemSoftwareVersion",
            "titleName",
            "container-size",
            "mandatory-size",
            "body-offset",
            "body-size",
            "body-digest",
            "pfs-offset-align",
            "pfs-size-align",
            "pfs-image-offset",
            "pfs-image-size",
            "fixed-info-size",
            "pfs-image-seed",
            "sblock-digest",
            "fixed-info-digest",
            "mount-image-offset",
            "mount-image-size",
            "container-offset",
            "supplemental-offset",
            "entry",
            "contentid",
            "languages",
            "scenarios",
            "scenario",
            "overall",
            "default",
            "chunks",
            "chunk",
            "outers",
            "outer",
            "sblock",
            "image-size",
            "super-inode",
            "inode",
            "seed",
            "icv",
            "metadata",
            "root",
            "file",
            "dir",
        ] {
            assert!(tags.iter().any(|t| t == element), "missing <{element}>");
        }
    }

    #[test]
    fn the_geometry_is_the_builds_own() {
        let xml = sample();
        assert!(
            xml.contains("<package-size>1507328</package-size>"),
            "si offset"
        );
        assert!(xml.contains("<container-offset>0x0000000000160000</container-offset>"));
        assert!(xml.contains("<mount-image-size>0x0000000000170000</mount-image-size>"));
        assert!(xml.contains("<container-size>0x0000000000010000</container-size>"));
        assert!(xml.contains("<pfs-image-size>0x0000000000150000</pfs-image-size>"));
        assert!(xml.contains("<content-type>PS5GD</content-type>"));
        assert!(xml.contains("<longname>UP0000-PPSA01234_00-TESTGAME00000000-C00000000-M0000000000000000-GD</longname>"));
        // The firmware word's dotted form, and the title escaped for XML.
        assert!(
            xml.contains("<required-system-version>11.600.000.00000000</required-system-version>")
        );
        assert!(xml.contains("<titleName>Test &amp; Co</titleName>"));
        // The mount root keeps its name even though the plan's own path for it is empty.
        assert!(xml.contains("name=\"uroot\">"), "uroot");
        assert!(xml.contains("name=\"sce_sys\">"));
        assert!(xml.contains("<entries nth-of-image=\"1\" num=\"2\">"));
        // One chunk spanning the container, as `playgo-chunk.dat` declares.
        assert!(xml.contains("init-size=\"1441792\" total=\"1441792\""));
        assert!(xml.contains("size=\"0x0000000000160000\" chunks=\"1\"/>"));
    }

    #[test]
    fn the_version_forms_match_the_samples() {
        assert_eq!(dotted_version(0x0100_0000), "01.000.000");
        assert_eq!(dotted_version(0x0104_4000), "01.044.000");
        assert_eq!(dotted_firmware("0x1160000000000000"), "11.600.000.00000000");
        assert_eq!(dotted_firmware("0x0100000000000000"), "01.000.000.00000000");
    }

    #[test]
    fn digest_lines_are_sixteen_bytes_each() {
        let mut xml = String::new();
        digest(&mut xml, 4, "content-digest", &[0xAB; 32]);
        let lines: Vec<&str> = xml.lines().collect();
        assert_eq!(lines.len(), 4, "{xml}");
        assert!(lines[0].starts_with("    <content-digest>"));
        assert_eq!(lines[1].matches("0xab").count(), 16);
        assert_eq!(lines[3], "    </content-digest>");
    }

    #[test]
    fn hex_words_are_bare_and_upper_case() {
        assert_eq!(hex_upper(&[0x47, 0xda, 0x42, 0x6a]), "47DA426A");
    }
}
