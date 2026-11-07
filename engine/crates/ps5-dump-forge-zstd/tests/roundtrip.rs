use std::collections::HashMap;
use std::io::Cursor;

use ps5_dump_forge_zstd::{
    Chunk, Codec, Index, VOLUME_HEADER_SIZE, decode_index, decode_volume_header, volume_name,
    writer,
};
use ps5upload_fpkg::source::{SourceFile, SourceTree};
use ps5upload_fpkg::{Error, Result};

struct Mem {
    files: Vec<SourceFile>,
    bytes: HashMap<String, Vec<u8>>,
}

impl Mem {
    fn new(bytes: HashMap<String, Vec<u8>>) -> Self {
        let mut files: Vec<_> = bytes
            .iter()
            .map(|(path, bytes)| SourceFile {
                path: path.clone(),
                size: bytes.len() as u64,
            })
            .collect();
        files.sort_by(|a, b| a.path.cmp(&b.path));
        Self { files, bytes }
    }
}

impl SourceTree for Mem {
    fn files(&self) -> &[SourceFile] {
        &self.files
    }
    fn read(&mut self, path: &str) -> Result<Vec<u8>> {
        self.bytes
            .get(path)
            .cloned()
            .ok_or_else(|| Error::Format(path.into()))
    }
    fn read_range(&mut self, path: &str, offset: u64, len: usize) -> Result<Vec<u8>> {
        let bytes = self
            .bytes
            .get(path)
            .ok_or_else(|| Error::Format(path.into()))?;
        let start = (offset as usize).min(bytes.len());
        Ok(bytes[start..start.saturating_add(len).min(bytes.len())].to_vec())
    }
    fn describe(&self) -> String {
        "memory".into()
    }
}

#[derive(Default)]
struct Output(HashMap<String, Vec<u8>>);

impl writer::PackOutput for Output {
    type W = Cursor<Vec<u8>>;
    fn create(&mut self, _: &str) -> Result<Self::W> {
        Ok(Cursor::new(Vec::new()))
    }
    fn reopen(&mut self, name: &str) -> Result<Self::W> {
        Ok(Cursor::new(self.0.remove(name).unwrap()))
    }
    fn finish(&mut self, name: &str, writer: Self::W) -> Result<()> {
        self.0.insert(name.into(), writer.into_inner());
        Ok(())
    }
}

const CHUNK: usize = ps5_dump_forge_zstd::CHUNK_SIZE as usize;

/// Deterministic incompressible bytes (xorshift64).
fn noise(len: usize, mut state: u64) -> Vec<u8> {
    (0..len)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state as u8
        })
        .collect()
}

fn pack(files: &[(&str, Vec<u8>)]) -> (Index, HashMap<String, Vec<u8>>) {
    let mut source = Mem::new(
        files
            .iter()
            .map(|(path, bytes)| (path.to_string(), bytes.clone()))
            .collect(),
    );
    let listed = source.files().to_vec();
    let mut output = Output::default();
    let index = writer::pack(&mut source, &listed, 5, &mut output).unwrap();
    (index, output.0)
}

fn chunks_of<'a>(index: &'a Index, path: &str) -> &'a [Chunk] {
    let file = index.files.iter().find(|file| file.path == path).unwrap();
    let first = file.first_chunk as usize;
    let count = file.logical_size.div_ceil(CHUNK as u64) as usize;
    &index.chunks[first..first + count]
}

/// Test-only decoder: the index and every volume header checked, then each
/// file rebuilt from its chunks (raw bytes, or one zstd frame each).
fn unpack(packs: &HashMap<String, Vec<u8>>) -> Result<(Index, HashMap<String, Vec<u8>>)> {
    let bad = |what: String| Error::Format(what);
    let index = decode_index(&packs["ampr_zstd.index"]).map_err(|e| bad(e.to_string()))?;
    for (id, &size) in index.volume_sizes.iter().enumerate() {
        let volume = &packs[&volume_name(id as u32)];
        let header = decode_volume_header(volume).map_err(|e| bad(e.to_string()))?;
        assert_eq!(header.id, id as u32);
        assert_eq!(header.count as usize, index.volume_sizes.len());
        assert_eq!(header.archive_id, index.archive_id);
        assert_eq!(header.payload_bytes, size - VOLUME_HEADER_SIZE as u64);
        assert_eq!(volume.len() as u64, size);
    }
    let mut files = HashMap::new();
    for file in &index.files {
        let mut data = Vec::with_capacity(file.logical_size as usize);
        for (n, chunk) in chunks_of(&index, &file.path).iter().enumerate() {
            let volume = &packs[&volume_name(chunk.volume_id.into())];
            let stored = &volume[chunk.offset as usize..][..chunk.stored_size as usize];
            let logical = (file.logical_size as usize - n * CHUNK).min(CHUNK);
            let bytes = match chunk.codec {
                Codec::Raw => stored.to_vec(),
                Codec::Zstd => zstd::bulk::decompress(stored, CHUNK)?,
            };
            assert_eq!(bytes.len(), logical, "{} chunk {n}", file.path);
            data.extend(bytes);
        }
        files.insert(file.path.clone(), data);
    }
    Ok((index, files))
}

#[test]
fn packed_file_and_damaged_volume() {
    let data: Vec<_> = (0..600_000).map(|i| (i % 7) as u8).collect();
    let (index, physical) = pack(&[("data/a.bin", data.clone())]);
    assert_eq!(physical["ampr_zstd.index"][16], 18);
    assert_eq!(chunks_of(&index, "data/a.bin").len(), 3);
    let (decoded, files) = unpack(&physical).unwrap();
    assert_eq!(decoded, index);
    assert_eq!(files["data/a.bin"], data);
    let mut damaged = physical.clone();
    let chunk = index
        .chunks
        .iter()
        .find(|chunk| chunk.codec == Codec::Zstd)
        .unwrap();
    let volume = damaged
        .get_mut(&volume_name(chunk.volume_id.into()))
        .unwrap();
    // The frame's last byte is its checksum.
    volume[chunk.offset as usize + chunk.stored_size as usize - 1] ^= 1;
    assert!(unpack(&damaged).is_err());
}

#[test]
fn exact_chunk_empty_file_and_raw_fallback() {
    let exact: Vec<_> = (0..CHUNK).map(|i| (i / 100) as u8).collect();
    // Compressible, incompressible, compressible: the file is not store-only,
    // but its middle chunk does not shrink and must fall back to raw.
    let mut mixed = vec![0u8; CHUNK];
    mixed.extend(noise(CHUNK, 0x9e37_79b9_7f4a_7c15));
    mixed.extend(vec![1u8; 1000]);
    let (index, physical) = pack(&[
        ("exact.bin", exact.clone()),
        ("empty.bin", Vec::new()),
        ("mixed.bin", mixed.clone()),
    ]);

    let exact_chunks = chunks_of(&index, "exact.bin");
    assert_eq!(exact_chunks.len(), 1);
    assert_eq!(exact_chunks[0].codec, Codec::Zstd);
    assert!(chunks_of(&index, "empty.bin").is_empty());
    let mixed_chunks = chunks_of(&index, "mixed.bin");
    let codecs: Vec<_> = mixed_chunks.iter().map(|chunk| chunk.codec).collect();
    assert_eq!(codecs, [Codec::Zstd, Codec::Raw, Codec::Zstd]);
    assert_eq!(mixed_chunks[1].stored_size as usize, CHUNK);
    let record = index.files.iter().find(|f| f.path == "mixed.bin").unwrap();
    assert!(!record.store_only);

    for chunk in index
        .chunks
        .iter()
        .filter(|chunk| chunk.codec == Codec::Zstd)
    {
        let volume = &physical[&volume_name(chunk.volume_id.into())];
        let frame = &volume[chunk.offset as usize..][..chunk.stored_size as usize];
        let descriptor = frame[4];
        assert_eq!(descriptor & 0x07, 0x04, "checksum on, no dictionary");
        if descriptor & 0x20 == 0 {
            let base = 1usize << (10 + (frame[5] >> 3));
            assert!(base + base / 8 * usize::from(frame[5] & 7) <= CHUNK);
        }
    }

    let (_, files) = unpack(&physical).unwrap();
    assert_eq!(files["exact.bin"], exact);
    assert_eq!(files["empty.bin"], b"");
    assert_eq!(files["mixed.bin"], mixed);
}

#[test]
fn a_listed_file_not_as_in_the_source_fails_before_writing() {
    let mut source = Mem::new(
        [
            ("a.bin".to_string(), vec![7u8; CHUNK + 10]),
            ("b.bin".to_string(), vec![8u8; 1000]),
        ]
        .into_iter()
        .collect(),
    );
    let listed = source.files().to_vec();
    let at = |size: u64| SourceFile {
        path: "a.bin".into(),
        size,
    };
    let size = listed[0].size;
    let cases = [
        ("smaller", vec![at(size - 1), listed[1].clone()]),
        ("larger", vec![at(size + 1), listed[1].clone()]),
        ("one chunk less", vec![at(size - 10), listed[1].clone()]),
        (
            "missing",
            vec![
                listed[0].clone(),
                SourceFile {
                    path: "c.bin".into(),
                    size: 1000,
                },
            ],
        ),
        (
            "duplicate",
            vec![listed[0].clone(), listed[1].clone(), listed[0].clone()],
        ),
    ];
    for (case, files) in cases {
        let mut output = Output::default();
        let error = writer::pack(&mut source, &files, 5, &mut output).unwrap_err();
        assert!(
            error.to_string().contains("invalid or changed packed file"),
            "{case}: {error}"
        );
        assert!(output.0.is_empty(), "{case}");
    }
    // The listing as it is packs.
    writer::pack(&mut source, &listed, 5, &mut Output::default()).unwrap();
}

#[test]
fn measured_packed_tree_reads_as_the_packs_pack_writes() {
    use ps5_dump_forge_zstd::tree::PackedTree;
    use std::sync::atomic::AtomicBool;
    let mut mixed = vec![3u8; 5 * CHUNK];
    mixed.extend(noise(2 * CHUNK, 5));
    mixed.extend((0..CHUNK + 9).map(|i| (i % 13) as u8));
    let files: HashMap<String, Vec<u8>> = [
        ("a/noise.bin", noise(3 * CHUNK + 77, 11)),
        ("b/mixed.bin", mixed),
        ("c/empty.bin", Vec::new()),
        ("d/small.txt", b"hello hello hello hello hello".repeat(40)),
        ("eboot.bin", vec![9u8; 5000]),
    ]
    .into_iter()
    .map(|(path, bytes)| (path.to_string(), bytes))
    .collect();
    let cancel = AtomicBool::new(false);
    let packed = |source: &Mem| -> Vec<SourceFile> {
        let all = source.files().iter();
        all.filter(|file| file.path != "eboot.bin")
            .cloned()
            .collect()
    };
    let mut source = Mem::new(files.clone());
    let listed = packed(&source);
    let mut output = Output::default();
    let written = writer::pack(&mut source, &listed, 5, &mut output).unwrap();
    let mut index = writer::measure(&mut source, &listed, 5, 3, &mut |_| {}, &cancel).unwrap();
    // The archive ID is random per run; everything else is the pack's.
    index.archive_id = written.archive_id;
    assert_eq!(index, written);
    let mut expected = output.0;
    expected.insert("eboot.bin".into(), files["eboot.bin"].clone());
    let mut tree = PackedTree::new(&mut source, &index, 3, &cancel).unwrap();
    let mut listed: Vec<_> = tree
        .files()
        .iter()
        .map(|f| (f.path.clone(), f.size))
        .collect();
    let mut want: Vec<_> = expected
        .iter()
        .map(|(path, bytes)| (path.clone(), bytes.len() as u64))
        .collect();
    listed.sort();
    want.sort();
    assert_eq!(listed, want);
    for (path, bytes) in &expected {
        assert_eq!(&tree.read(path).unwrap(), bytes, "{path}");
        // In odd pieces, so a read starts and ends inside chunks and headers.
        let mut pieces = Vec::new();
        while pieces.len() < bytes.len() {
            pieces.extend(tree.read_range(path, pieces.len() as u64, 100_003).unwrap());
        }
        assert_eq!(&pieces, bytes, "{path} in pieces");
    }
}

/// A packed file also on disk is never refused: an identical copy keeps the plain file, another
/// (here another case and other bytes) is replaced by the packed copy under the index's name.
#[test]
fn a_packed_file_also_on_disk_keeps_one_copy() {
    let a = noise(1000, 1);
    let b = noise(2000, 2);
    let (_, mut physical) = pack(&[("data/a.bin", a.clone()), ("data/b.bin", b.clone())]);
    physical.insert("data/a.bin".into(), a.clone());
    physical.insert("DATA/B.bin".into(), vec![9; 10]);
    let mut tree = ps5_dump_forge_zstd::reader::unpack(Box::new(Mem::new(physical))).unwrap();
    assert_eq!(tree.identical_paths(), ["data/a.bin"]);
    assert_eq!(tree.differing_paths(), ["data/b.bin"]);
    let listed: Vec<(&str, u64)> = (tree.files().iter())
        .map(|f| (f.path.as_str(), f.size))
        .collect();
    assert_eq!(listed, [("data/a.bin", 1000), ("data/b.bin", 2000)]);
    assert_eq!(tree.read("data/a.bin").unwrap(), a);
    assert_eq!(tree.read("data/b.bin").unwrap(), b);
    assert!(tree.read("DATA/B.bin").is_err());
}

/// Two records folding to one path, one of them identical to the file on disk: both stay
/// packed, so neither loses its contents to the other.
#[test]
fn records_folding_together_both_keep_their_packed_copy() {
    let a = noise(1000, 3);
    let b = noise(1000, 4);
    let (_, mut physical) = pack(&[("data/a.bin", a.clone()), ("DATA/A.bin", b.clone())]);
    physical.insert("data/a.bin".into(), a.clone());
    let mut tree = ps5_dump_forge_zstd::reader::unpack(Box::new(Mem::new(physical))).unwrap();
    assert!(tree.identical_paths().is_empty());
    assert_eq!(tree.read("data/a.bin").unwrap(), a);
    assert_eq!(tree.read("DATA/A.bin").unwrap(), b);
    assert_eq!(tree.files().len(), 2);
}
