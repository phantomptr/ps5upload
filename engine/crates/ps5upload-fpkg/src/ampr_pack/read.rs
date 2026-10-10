//! Reading a pack set back: the manifest validated, every volume's header checked against it,
//! and a file's bytes decoded chunk by chunk — CRC-checked when the sidecar is at hand.

use super::format::*;
use super::lz4;
use crate::source::SourceTree;
use crate::{format_err, Result};

pub struct Reader {
    pub manifest: Manifest,
    crcs: Option<Vec<u32>>,
    payload: Vec<(u64, u64)>,
}

impl Reader {
    /// Open the pack set whose manifest is `index_name` in `tree` (a folder, an image), with its
    /// `.crc` sidecar when the tree has one.
    pub fn open(tree: &mut dyn SourceTree, index_name: &str) -> Result<Self> {
        let manifest = Manifest::parse(&tree.read(index_name)?)?;
        let crc_name = format!("{index_name}.crc");
        let crcs = if tree.files().iter().any(|f| f.path == crc_name) {
            Some(parse_crc_sidecar(
                &tree.read(&crc_name)?,
                &manifest.build_id,
                manifest.chunks.len(),
            )?)
        } else {
            None
        };
        let dir = index_name.rsplit_once('/').map_or("", |(d, _)| d);
        let mut payload = Vec::with_capacity(manifest.packs.len());
        for (id, record) in manifest.packs.iter().enumerate() {
            let name = join(dir, manifest.pack_name(id)?);
            let size = tree
                .files()
                .iter()
                .find(|f| f.path == name)
                .map(|f| f.size)
                .ok_or_else(|| crate::Error::Format(format!("pack volume {name} is missing")))?;
            let (pack_id, build, offset, bytes, flags) =
                parse_data_header(&tree.read_range(&name, 0, DATA_HEADER_SIZE)?)?;
            if pack_id as usize != id || build != manifest.build_id || flags != record.flags {
                return format_err(format!("{name} belongs to another pack set"));
            }
            if size != record.file_size || bytes != record.payload_bytes || offset + bytes != size {
                return format_err(format!("{name} does not have the size the manifest says"));
            }
            payload.push((offset, offset + bytes));
        }
        Ok(Self {
            manifest,
            crcs,
            payload,
        })
    }

    /// Record index (file id − 1) of a path relative to `/app0`, case-insensitively as AMPR
    /// looks paths up.
    pub fn find(&self, relative: &str) -> Option<usize> {
        let want = path_hash(&format!("/app0/{}", relative.trim_start_matches('/')));
        (0..self.manifest.files.len()).find(|&i| {
            self.manifest.files[i].path_hash == want
                && self
                    .manifest
                    .path(i)
                    .is_ok_and(|p| p[6..].eq_ignore_ascii_case(relative.trim_start_matches('/')))
        })
    }

    /// The decoded bytes of record `i`, reading volumes from `tree` (named relative to the
    /// manifest's folder `dir`). A loose record is an error: its bytes are not in the packs.
    pub fn read_file(&self, i: usize, tree: &mut dyn SourceTree, dir: &str) -> Result<Vec<u8>> {
        let f = &self.manifest.files[i];
        if !f.packed() {
            return format_err(format!("{} is loose", self.manifest.path(i)?));
        }
        let mut out = Vec::with_capacity(f.logical_size as usize);
        for local in 0..f.chunk_count {
            let index = f.first_chunk as usize + local as usize;
            let c = &self.manifest.chunks[index];
            let raw_size = Manifest::raw_size(f, local) as usize;
            let (begin, end) = self.payload[usize::from(c.pack_id)];
            if c.offset < begin || c.offset + u64::from(c.stored_size) > end {
                return format_err("a chunk lies outside its volume's payload");
            }
            let name = join(dir, self.manifest.pack_name(usize::from(c.pack_id))?);
            let stored = tree.read_range(&name, c.offset, c.stored_size as usize)?;
            if stored.len() != c.stored_size as usize {
                return format_err(format!("{name} is truncated"));
            }
            let raw = match c.codec {
                CODEC_RAW => stored,
                CODEC_LZ4 => lz4::decompress(&stored, raw_size)?,
                other => return format_err(format!("unknown chunk codec {other}")),
            };
            if raw.len() != raw_size {
                return format_err("a chunk decoded to the wrong size");
            }
            if let Some(crcs) = &self.crcs {
                if crc32(&raw) != crcs[index] {
                    return format_err(format!(
                        "chunk {index} of {} fails its CRC",
                        self.manifest.path(i)?
                    ));
                }
            }
            out.extend_from_slice(&raw);
        }
        if out.len() as u64 != f.logical_size {
            return format_err("a packed file decoded to the wrong size");
        }
        Ok(out)
    }

    pub fn has_crcs(&self) -> bool {
        self.crcs.is_some()
    }
}

fn join(dir: &str, name: &str) -> String {
    if dir.is_empty() {
        name.to_string()
    } else {
        format!("{dir}/{name}")
    }
}
