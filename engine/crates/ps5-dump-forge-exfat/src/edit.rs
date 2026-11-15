//! In-place editor for an existing `.exfat` image. This slice reads the volume: boot region,
//! FAT, allocation bitmap, up-case table and the directory tree; [`plan`] turns ops into writes
//! (deleting, renaming or moving a file or folder, adding or replacing a file, a new folder).
//!
//! The FAT (4 B per cluster) and the bitmap are held in memory whole, a plan's directory listings
//! beside them while they fit, within [`MEMORY_LIMIT`].

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};

use ps5_dump_forge_patch::{Data, Op, Write};

use crate::{
    ALLOCATION_POSSIBLE, ATTR_ARCHIVE, ATTR_DIRECTORY, ENTRY, FAT_EOC, FIRST_CLUSTER,
    MAX_DIR_BYTES, NO_FAT_CHAIN, TIMESTAMP, names,
};

/// FAT + bitmap + the directory being read, at most.
// ponytail: 128 MiB holds a 1 TiB image of 64 KiB clusters (~64 MiB of FAT); a bigger or
// finer-clustered volume is refused (rebuild instead), where windowed reads would lift it.
pub const MEMORY_LIMIT: u64 = 128 << 20;

/// An opened exFAT volume, its metadata read into memory.
#[derive(Debug, Clone)]
pub struct Volume {
    pub sector_shift: u8,
    pub cluster_shift: u8,
    /// VolumeLength, in bytes.
    pub volume_len: u64,
    /// Byte offset and length of the (only) FAT.
    pub fat_offset: u64,
    pub fat_len: u64,
    /// Byte offset of cluster 2.
    pub heap_offset: u64,
    pub cluster_count: u32,
    pub root_cluster: u32,
    /// VolumeFlags' VolumeDirty bit.
    pub dirty: bool,
    /// FAT entries 0..cluster_count + 2.
    pub fat: Vec<u32>,
    /// The allocation bitmap, bit n for cluster n + 2.
    pub bitmap: Vec<u8>,
    /// The bitmap's and the up-case table's own streams (root directory entries).
    pub bitmap_stream: Stream,
    pub upcase_stream: Stream,
    /// The image's up-case table expanded, or `None` when it is the standard one.
    upcase: Option<Vec<u16>>,
    /// What [`plan`]'s ops changed so far; reads see it.
    changes: Changes,
    /// [`MEMORY_LIMIT`] (lowered by tests).
    limit: u64,
}

/// The planned change set: modified copies of whole sectors, each written once.
#[derive(Debug, Clone, Default)]
struct Changes {
    /// Directory sectors by byte offset in the image.
    dir: BTreeMap<u64, Vec<u8>>,
    /// FAT sectors (index from the FAT's start) whose entries in [`Volume::fat`] changed.
    fat: BTreeSet<u64>,
    /// Bitmap sectors (index from the bitmap stream's start) changed in [`Volume::bitmap`].
    bitmap: BTreeSet<u64>,
    /// Directory clusters this plan allocates, their whole content (written as free space).
    fresh: BTreeMap<u32, Vec<u8>>,
    /// The bitmap as it was before the first cluster was freed, with every cluster this plan
    /// frees marked too: only clusters free in both are allocated, so a free-space write never
    /// lands on bytes the current filesystem uses, nor on another free-space write.
    before: Option<Vec<u8>>,
    /// Bytes of the folders a delete holds in memory, waiting to be listed.
    held: u64,
    /// New files' data and zeroed tails, as free-space writes, and the bytes they hold.
    data: Vec<Write>,
    data_bytes: u64,
    /// Free clusters when the volume was opened, those this plan took, and those it could not
    /// take (no room: one finding at the end, "the changes need N MB ...").
    free: u64,
    taken: u64,
    short: u64,
    /// Folders (up-cased paths) not made for want of room, so a later op counts them once.
    missing: BTreeSet<Vec<u16>>,
    /// Where the next allocation search starts (next fit: files laid out one after another).
    cursor: u32,
    /// Folders listed so far, kept in step with the writes above.
    listings: RefCell<Listings>,
}

/// A folder's entries as [`Volume::read_dir`] would read them now: read once per plan, the
/// entries written since re-read before its next use (so N puts into one folder don't read
/// it N times).
#[derive(Debug, Clone, Default)]
struct Listing {
    clusters: Vec<u32>,
    /// Each entry's EntryType, and the index of the first end-of-directory entry (or the count).
    kinds: Vec<u8>,
    stop: usize,
    /// The entry sets before `stop` by their File entry's index, each with its up-cased name;
    /// and by that name (a damaged folder may repeat one: the first is found, as a scan would).
    sets: BTreeMap<usize, (Node, Vec<u16>)>,
    names: HashMap<Vec<u16>, BTreeSet<usize>>,
    /// Entries written since they were read.
    dirty: BTreeSet<usize>,
    /// What it holds, roughly.
    bytes: u64,
}

/// The [`Listing`]s held, charged within [`MEMORY_LIMIT`] when added (dropped to make room).
#[derive(Debug, Clone, Default)]
struct Listings {
    /// By the folder's first cluster.
    by_first: HashMap<u32, Listing>,
    /// Each cluster of a held folder: that folder's first cluster and the cluster's place in it.
    owner: HashMap<u32, (u32, usize)>,
    bytes: u64,
}

/// What a set held in a [`Listing`] costs: the node, its up-cased name, two map slots.
fn set_bytes(n: &Node, up: &[u16]) -> u64 {
    node_bytes(n) + up.len() as u64 * 4 + 64
}

impl Listing {
    fn new(clusters: &[u32], kinds: Vec<u8>, nodes: Vec<Node>, v: &Volume) -> Listing {
        let mut l = Listing {
            clusters: clusters.to_vec(),
            stop: kinds.iter().position(|&k| k == 0x00).unwrap_or(kinds.len()),
            bytes: (clusters.len() * 4 + kinds.len()) as u64,
            kinds,
            ..Listing::default()
        };
        let per = (v.cluster_size() / ENTRY as u64) as usize;
        for n in nodes {
            let i = l.index(n.entries[0], v, per);
            let up = v.upcased(&n.name);
            l.insert(i, n, up);
        }
        l
    }

    /// The entry index of image byte `at` (within this folder's clusters).
    fn index(&self, at: u64, v: &Volume, per: usize) -> usize {
        let c = ((at - v.heap_offset) / v.cluster_size()) as u32 + FIRST_CLUSTER as u32;
        let k = self.clusters.iter().position(|&x| x == c).unwrap_or(0);
        k * per + ((at - v.cluster_offset(c)) / ENTRY as u64) as usize
    }

    fn insert(&mut self, i: usize, n: Node, up: Vec<u16>) {
        self.bytes += set_bytes(&n, &up);
        self.names.entry(up.clone()).or_default().insert(i);
        self.sets.insert(i, (n, up));
    }

    fn remove(&mut self, i: usize) {
        let Some((n, up)) = self.sets.remove(&i) else {
            return;
        };
        self.bytes -= set_bytes(&n, &up).min(self.bytes);
        if let Some(at) = self.names.get_mut(&up) {
            at.remove(&i);
            if at.is_empty() {
                self.names.remove(&up);
            }
        }
    }

    /// The first set named `up` (up-cased).
    fn find(&self, up: &[u16]) -> Option<&Node> {
        let &i = self.names.get(up)?.first()?;
        self.sets.get(&i).map(|(n, _)| n)
    }
}

impl Listings {
    fn drop_listing(&mut self, first: u32) {
        if let Some(l) = self.by_first.remove(&first) {
            for c in &l.clusters {
                self.owner.remove(c);
            }
            self.bytes -= l.bytes.min(self.bytes);
        }
    }

    /// Drops the listing holding cluster `c`, if one does.
    fn drop_cluster(&mut self, c: u32) {
        if let Some(&(first, _)) = self.owner.get(&c) {
            self.drop_listing(first);
        }
    }

    fn add(&mut self, l: Listing) {
        // A cluster in two folders (a damaged volume): only the newer listing is kept.
        for &c in &l.clusters {
            self.drop_cluster(c);
        }
        let first = l.clusters[0];
        for (k, &c) in l.clusters.iter().enumerate() {
            self.owner.insert(c, (first, k));
        }
        self.bytes += l.bytes;
        self.by_first.insert(first, l);
    }
}

/// Where a stream's data lives.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Stream {
    pub first_cluster: u32,
    /// DataLength in bytes.
    pub len: u64,
    pub no_fat_chain: bool,
}

/// One file or directory entry set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Node {
    pub name: String,
    /// FileAttributes of the File entry.
    pub attributes: u16,
    pub stream: Stream,
    pub valid_len: u64,
    /// The directory cluster holding the File entry, and the absolute byte offset of each
    /// entry of the set (File, Stream Extension, File Names), in order.
    pub cluster: u32,
    pub entries: Vec<u64>,
    /// The set holds benign secondaries past its names (Vendor Extension/Allocation): kept on
    /// delete, but a rename would have to carry them.
    pub vendor_entries: bool,
    /// The clusters those secondaries own (Vendor Allocation), freed with the set.
    pub vendor_allocations: Vec<Stream>,
}

impl Node {
    pub fn is_dir(&self) -> bool {
        self.attributes & ATTR_DIRECTORY != 0
    }
}

impl Volume {
    pub fn cluster_size(&self) -> u64 {
        1 << (self.sector_shift + self.cluster_shift)
    }

    /// Byte offset of heap cluster `c` (≥ 2).
    pub fn cluster_offset(&self, c: u32) -> u64 {
        self.heap_offset + (u64::from(c) - FIRST_CLUSTER) * self.cluster_size()
    }

    /// Whether the bitmap marks cluster `c` (≥ 2) in use.
    pub fn in_use(&self, c: u32) -> bool {
        let i = (c - FIRST_CLUSTER as u32) as usize;
        self.bitmap[i / 8] & (1 << (i % 8)) != 0
    }

    pub fn free_clusters(&self) -> u64 {
        (FIRST_CLUSTER as u32..self.cluster_count + FIRST_CLUSTER as u32)
            .filter(|&c| !self.in_use(c))
            .count() as u64
    }

    /// Free clusters × cluster size.
    pub fn free_bytes(&self) -> u64 {
        self.free_clusters() * self.cluster_size()
    }

    fn valid(&self, c: u32) -> bool {
        (FIRST_CLUSTER as u32..self.cluster_count + FIRST_CLUSTER as u32).contains(&c)
    }

    /// The clusters of `s` in order: a contiguous run under NoFatChain, else its FAT chain.
    /// A stream of `len` 0 owns none. A chain that loops, leaves the heap or ends early is an
    /// error, and so is a list of clusters (4 B each) past what [`MEMORY_LIMIT`] leaves.
    pub fn chain(&self, s: Stream) -> io::Result<Vec<u32>> {
        self.chain_within(s, 4)
    }

    /// A directory's clusters: each costs its list entry and the cluster read beside it.
    fn dir_chain(&self, s: Stream) -> io::Result<Vec<u32>> {
        self.chain_within(s, 4 + self.cluster_size())
    }

    /// [`Self::chain`], refused up front when `n` clusters at `per` bytes each pass the budget.
    fn chain_within(&self, s: Stream, per: u64) -> io::Result<Vec<u32>> {
        let n = s.len.div_ceil(self.cluster_size());
        if n == 0 {
            return Ok(Vec::new());
        }
        if n > u64::from(self.cluster_count) || !self.valid(s.first_cluster) {
            return Err(bad(format!(
                "stream at cluster {} out of range",
                s.first_cluster
            )));
        }
        if n > self.remaining() / per {
            return Err(too_big(self.tables_bytes() + n * per));
        }
        if s.no_fat_chain {
            let last = u64::from(s.first_cluster) + n - 1;
            if last >= u64::from(self.cluster_count) + FIRST_CLUSTER {
                return Err(bad(format!(
                    "run at cluster {} out of range",
                    s.first_cluster
                )));
            }
            return Ok((s.first_cluster..=last as u32).collect());
        }
        self.walk(s.first_cluster, Some(n), n)
    }

    /// A directory chain followed to its end of chain (the root has no DataLength), at most
    /// what the budget leaves for a directory's clusters.
    fn whole_chain(&self, first: u32) -> io::Result<Vec<u32>> {
        let cap = self.remaining() / (4 + self.cluster_size());
        self.walk(first, None, cap)
    }

    /// The FAT chain from `first` to its end of chain, which must come after exactly `want`
    /// clusters when given. A free, bad or out-of-range entry before the end, a chain longer
    /// or shorter than `want`, or one longer than the cluster count (a loop: no HashSet, the
    /// length says it) is an error; past `cap` clusters the walk stops as too big.
    fn walk(&self, first: u32, want: Option<u64>, cap: u64) -> io::Result<Vec<u32>> {
        // Reserved once, exactly: the loop below never pushes past `cap` (or the cluster
        // count), so the list never regrows past its bound.
        let cap = cap.min(u64::from(self.cluster_count));
        let mut out = Vec::with_capacity(cap as usize);
        let mut c = first;
        loop {
            let len = out.len() as u64;
            if len >= u64::from(self.cluster_count) {
                return Err(bad(format!("FAT chain of cluster {first} loops")));
            }
            if len >= cap {
                return Err(too_big(
                    self.tables_bytes() + (len + 1) * (4 + self.cluster_size()),
                ));
            }
            if !self.valid(c) {
                return Err(bad(format!("FAT chain of cluster {first} breaks")));
            }
            out.push(c);
            let next = self.fat[c as usize];
            let len = len + 1;
            match want {
                _ if next == FAT_EOC && want.is_none_or(|n| n == len) => return Ok(out),
                Some(n) if next == FAT_EOC || n == len => {
                    return Err(bad(format!(
                        "FAT chain of cluster {first} does not match its {n}-cluster stream"
                    )));
                }
                _ => c = next,
            }
        }
    }

    /// The stream's `len` bytes, read cluster by cluster.
    pub fn read_stream(&self, image: &mut File, s: Stream) -> io::Result<Vec<u8>> {
        let clusters = self.chain(s)?;
        self.read_clusters(image, &clusters, s.len)
    }

    fn read_clusters(&self, image: &mut File, clusters: &[u32], len: u64) -> io::Result<Vec<u8>> {
        let cs = self.cluster_size();
        let mut out = vec![0u8; len as usize];
        for (i, chunk) in out.chunks_mut(cs as usize).enumerate() {
            let at = self.cluster_offset(clusters[i]);
            if let Some(fresh) = self.changes.fresh.get(&clusters[i]) {
                chunk.copy_from_slice(&fresh[..chunk.len()]);
                continue;
            }
            read_at(image, at, chunk)?;
            for (&off, sector) in self.changes.dir.range(at..at + chunk.len() as u64) {
                let from = (off - at) as usize;
                let n = sector.len().min(chunk.len() - from);
                chunk[from..from + n].copy_from_slice(&sector[..n]);
            }
        }
        Ok(out)
    }
}

fn read_at(image: &mut File, offset: u64, buf: &mut [u8]) -> io::Result<()> {
    image.seek(SeekFrom::Start(offset))?;
    image.read_exact(buf)
}

fn too_big(total: u64) -> io::Error {
    io::Error::new(
        io::ErrorKind::OutOfMemory,
        format!(
            "its allocation tables are too big to edit in place ({} MiB); rebuild it instead",
            total.div_ceil(1 << 20)
        ),
    )
}

fn bad(msg: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, format!("exFAT: {}", msg.into()))
}

fn le16(b: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([b[at], b[at + 1]])
}

fn le32(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(b[at..at + 4].try_into().unwrap())
}

fn le64(b: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(b[at..at + 8].try_into().unwrap())
}

impl Volume {
    /// Reads and checks the boot region (any valid sector and cluster shift; the boot
    /// checksum verified), the FAT, the allocation bitmap and the up-case table.
    pub fn open(image: &mut File) -> io::Result<Volume> {
        let mut s0 = [0u8; 512];
        read_at(image, 0, &mut s0)?;
        if &s0[3..11] != b"EXFAT   " || le16(&s0, 510) != 0xAA55 {
            return Err(bad("not an exFAT boot sector"));
        }
        let (ss, cs) = (s0[108], s0[109]);
        if !(9..=12).contains(&ss) || u32::from(ss) + u32::from(cs) > 25 {
            return Err(bad(format!("bad shifts: sector {ss}, cluster {cs}")));
        }
        if s0[110] != 1 {
            return Err(bad(format!("{} FATs (only one is supported)", s0[110])));
        }
        let sector = 1usize << ss;
        let mut region = vec![0u8; 12 * sector];
        read_at(image, 0, &mut region)?;
        let sum = crate::boot_checksum(&region[..11 * sector]);
        if region[11 * sector..].chunks(4).any(|c| le32(c, 0) != sum) {
            return Err(bad("boot checksum mismatch"));
        }
        let cluster_count = le32(&s0, 92);
        let mut v = Volume {
            sector_shift: ss,
            cluster_shift: cs,
            volume_len: le64(&s0, 72) << ss,
            fat_offset: u64::from(le32(&s0, 80)) << ss,
            fat_len: u64::from(le32(&s0, 84)) << ss,
            heap_offset: u64::from(le32(&s0, 88)) << ss,
            cluster_count,
            root_cluster: le32(&s0, 96),
            dirty: le16(&s0, 106) & 0x2 != 0,
            fat: Vec::new(),
            bitmap: Vec::new(),
            bitmap_stream: Stream {
                first_cluster: 0,
                len: 0,
                no_fat_chain: false,
            },
            upcase_stream: Stream {
                first_cluster: 0,
                len: 0,
                no_fat_chain: false,
            },
            upcase: None,
            changes: Changes::default(),
            limit: MEMORY_LIMIT,
        };
        let fat_need = (u64::from(cluster_count) + FIRST_CLUSTER) * 4;
        let heap_end = v.heap_offset + u64::from(cluster_count) * v.cluster_size();
        let file_len = image.metadata()?.len();
        if cluster_count == 0
            || u64::from(cluster_count) > crate::MAX_CLUSTERS
            || v.fat_len < fat_need
            || v.fat_offset + v.fat_len > v.heap_offset
            || heap_end > v.volume_len
        {
            return Err(bad("inconsistent geometry (FAT, heap or volume length)"));
        }
        v.fits(0)?;
        if heap_end > file_len {
            return Err(bad(format!(
                "the image is {file_len} bytes, its cluster heap ends at {heap_end}"
            )));
        }
        // Decoded in 1 MiB reads: no second, byte copy of the FAT.
        v.fat = Vec::with_capacity(fat_need as usize / 4);
        let mut buf = vec![0u8; 1 << 20];
        let mut at = 0;
        while at < fat_need {
            let n = (fat_need - at).min(buf.len() as u64) as usize;
            read_at(image, v.fat_offset + at, &mut buf[..n])?;
            v.fat.extend(buf[..n].chunks(4).map(|c| le32(c, 0)));
            at += n as u64;
        }
        v.read_root_metadata(image)?;
        v.changes.free = v.free_clusters();
        v.changes.cursor = FIRST_CLUSTER as u32;
        Ok(v)
    }
}

impl Volume {
    /// Refuses a volume whose FAT and bitmap, with `dir_bytes` of directory data loaded beside
    /// them, would pass [`MEMORY_LIMIT`].
    fn fits(&self, dir_bytes: u64) -> io::Result<()> {
        let total = self.tables_bytes() + dir_bytes;
        // The listings kept count too; when only they leave no room, they go (a listing in use
        // is borrowed, and then none is counted).
        let kept = self.changes.listings.try_borrow().map_or(0, |h| h.bytes);
        if total + kept <= self.limit {
            return Ok(());
        }
        if kept > 0
            && let Ok(mut held) = self.changes.listings.try_borrow_mut()
        {
            *held = Listings::default();
        }
        if total <= self.limit {
            return Ok(());
        }
        Err(too_big(total))
    }

    /// The in-memory FAT and bitmap, and the sectors [`plan`]'s change set holds (or will
    /// copy into its writes, the boot sector's two included), in bytes.
    fn tables_bytes(&self) -> u64 {
        let cc = u64::from(self.cluster_count);
        let c = &self.changes;
        let changed = (c.dir.len() + c.fat.len() + c.bitmap.len() + 2) as u64;
        let before = c.before.as_ref().map_or(0, |b| b.len() as u64);
        (cc + FIRST_CLUSTER) * 4
            + cc.div_ceil(8)
            + before
            + c.held
            + changed * (1 << self.sector_shift)
            + c.fresh.len() as u64 * self.cluster_size()
            + c.data_bytes
            // A `Vec` grows by doubling: each write's slot counted twice.
            + c.data.len() as u64 * 2 * std::mem::size_of::<Write>() as u64
    }

    /// What [`MEMORY_LIMIT`] leaves beside the FAT and bitmap.
    fn remaining(&self) -> u64 {
        self.limit.saturating_sub(self.tables_bytes())
    }

    /// The root's Allocation Bitmap and Up-case Table entries, then their data.
    fn read_root_metadata(&mut self, image: &mut File) -> io::Result<()> {
        let clusters = self.whole_chain(self.root_cluster)?;
        self.fits(clusters.len() as u64 * self.cluster_size())?;
        let raw = self.read_clusters(
            image,
            &clusters,
            clusters.len() as u64 * self.cluster_size(),
        )?;
        let (mut bitmap, mut upcase) = (None, None);
        for e in raw.chunks(ENTRY) {
            let stream = Stream {
                first_cluster: le32(e, 20),
                len: le64(e, 24),
                no_fat_chain: false,
            };
            match e[0] {
                0x00 => break,
                // BitmapFlags bit 0 names the FAT; with one FAT only the first bitmap exists.
                0x81 if e[1] & 1 == 0 => bitmap = Some(stream),
                0x82 => upcase = Some((stream, le32(e, 4))),
                _ => {}
            }
        }
        let bitmap = bitmap.ok_or_else(|| bad("no allocation bitmap entry"))?;
        let (upcase, upcase_sum) = upcase.ok_or_else(|| bad("no up-case table entry"))?;
        let need = u64::from(self.cluster_count).div_ceil(8);
        if bitmap.len < need
            || bitmap.len > need + self.cluster_size()
            || upcase.len == 0
            || upcase.len > 128 << 10
            || upcase.len % 2 != 0
        {
            return Err(bad("bad allocation bitmap or up-case table length"));
        }
        self.bitmap = self.read_stream(image, bitmap)?;
        self.bitmap.truncate(need as usize);
        self.bitmap_stream = bitmap;
        let table = self.read_stream(image, upcase)?;
        if names::table_checksum(&table) != upcase_sum {
            return Err(bad("up-case table checksum mismatch"));
        }
        if table != names::table_bytes() {
            let units: Vec<u16> = table.chunks(2).map(|c| le16(c, 0)).collect();
            self.upcase = Some(names::expand(&units));
        }
        self.upcase_stream = upcase;
        Ok(())
    }

    /// `name` through this volume's up-case table.
    pub fn upcased(&self, name: &str) -> Vec<u16> {
        match &self.upcase {
            None => names::upcase(name),
            Some(t) => name.encode_utf16().map(|u| t[usize::from(u)]).collect(),
        }
    }

    /// The entry sets of the directory at game path `dir` (`""` is the root).
    pub fn list(&self, image: &mut File, dir: &str) -> io::Result<Vec<Node>> {
        let clusters = if dir.is_empty() {
            self.whole_chain(self.root_cluster)?
        } else {
            match self.lookup(image, dir)? {
                Some(n) if n.is_dir() => self.dir_chain(n.stream)?,
                Some(_) => return Err(bad(format!("{dir}: not a directory"))),
                None => return Err(io::Error::new(io::ErrorKind::NotFound, dir.to_string())),
            }
        };
        self.read_dir(image, &clusters)
    }

    /// The node at game path `path` (`/`-separated, compared through the up-case table), or
    /// `None` when it does not exist (or a parent is a file).
    pub fn lookup(&self, image: &mut File, path: &str) -> io::Result<Option<Node>> {
        let parts: Vec<&str> = path.split('/').collect();
        if parts.iter().any(|p| p.is_empty()) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("bad path {path:?}"),
            ));
        }
        let mut clusters = self.whole_chain(self.root_cluster)?;
        for (i, part) in parts.iter().enumerate() {
            let want = self.upcased(part);
            let found = self.with_listing(image, &clusters, |l| l.find(&want).cloned())?;
            match found {
                Some(n) if i + 1 == parts.len() => return Ok(Some(n)),
                Some(n) if n.is_dir() => clusters = self.dir_chain(n.stream)?,
                _ => return Ok(None),
            }
        }
        unreachable!("split yields at least one part")
    }
}

impl Volume {
    /// The File entry sets of a directory held by `clusters`, each set checked (layout and
    /// SetChecksum). Deleted entries are skipped; an end-of-directory entry stops the scan.
    fn read_dir(&self, image: &mut File, clusters: &[u32]) -> io::Result<Vec<Node>> {
        Ok(self.scan_dir(image, clusters)?.1)
    }

    /// [`Self::read_dir`], with each entry's EntryType.
    fn scan_dir(&self, image: &mut File, clusters: &[u32]) -> io::Result<(Vec<u8>, Vec<Node>)> {
        let cs = self.cluster_size();
        self.fits(clusters.len() as u64 * cs)?;
        let raw = self.read_clusters(image, clusters, clusters.len() as u64 * cs)?;
        let per = (cs / ENTRY as u64) as usize;
        let at = |i: usize| self.cluster_offset(clusters[i / per]) + (i % per * ENTRY) as u64;
        let total = raw.len() / ENTRY;
        let mut out = Vec::new();
        // The listing is charged as it grows, beside the clusters it is read from.
        let mut held = raw.len() as u64;
        let mut i = 0;
        while i < total && raw[i * ENTRY] != 0x00 {
            if raw[i * ENTRY] != 0x85 {
                i += 1;
                continue;
            }
            let sc = usize::from(raw[i * ENTRY + 1]);
            if sc < 2 || i + sc >= total {
                return Err(bad(format!("bad entry set at byte {}", at(i))));
            }
            let set = &raw[i * ENTRY..(i + 1 + sc) * ENTRY];
            let node = parse_set(set, |k| at(i + k), clusters[i / per])?;
            held += node_bytes(&node);
            self.fits(held)?;
            out.push(node);
            i += 1 + sc;
        }
        Ok((raw.chunks(ENTRY).map(|e| e[0]).collect(), out))
    }

    /// The listing of the folder held by `clusters` (read when not held, or held for another
    /// chain), its written entries re-read, given to `f`. Held while it fits beside the rest.
    fn with_listing<R>(
        &self,
        image: &mut File,
        clusters: &[u32],
        f: impl FnOnce(&Listing) -> R,
    ) -> io::Result<R> {
        let Some(&first) = clusters.first() else {
            return Ok(f(&Listing::default()));
        };
        let mut held = self.changes.listings.borrow_mut();
        if held
            .by_first
            .get(&first)
            .is_some_and(|l| l.clusters != clusters)
        {
            held.drop_listing(first);
        }
        if let Some(mut l) = held.by_first.remove(&first) {
            held.bytes -= l.bytes.min(held.bytes);
            // Taken out while it is brought up to date; an error drops it.
            for c in &l.clusters {
                held.owner.remove(c);
            }
            self.refresh(image, &mut l)?;
            // Grown by the refresh: kept within the limit as a new one is.
            if self.tables_bytes() + held.bytes + l.bytes > self.limit {
                *held = Listings::default();
            }
            held.add(l);
            return Ok(f(&held.by_first[&first]));
        }
        drop(held);
        let (kinds, nodes) = self.scan_dir(image, clusters)?;
        let l = Listing::new(clusters, kinds, nodes, self);
        let mut held = self.changes.listings.borrow_mut();
        if self.tables_bytes() + held.bytes + l.bytes > self.limit {
            *held = Listings::default();
        }
        if self.tables_bytes() + l.bytes > self.limit {
            return Ok(f(&l));
        }
        held.add(l);
        Ok(f(&held.by_first[&first]))
    }

    /// Re-reads `l`'s written entries: their EntryTypes, the sets they were or are part of,
    /// and the sets an end-of-directory entry moved now hides or shows.
    fn refresh(&self, image: &mut File, l: &mut Listing) -> io::Result<()> {
        if l.dirty.is_empty() {
            return Ok(());
        }
        let per = (self.cluster_size() / ENTRY as u64) as usize;
        let clusters = std::mem::take(&mut l.clusters);
        let at = |i: usize| self.cluster_offset(clusters[i / per]) + (i % per * ENTRY) as u64;
        let mut again = std::mem::take(&mut l.dirty);
        for &j in &again.clone() {
            l.kinds[j] = self.read_entry(image, at(j))?[0];
            if let Some((&i, (n, _))) = l.sets.range(..=j).next_back()
                && i + n.entries.len() > j
            {
                again.insert(i);
            }
        }
        for &i in &again {
            l.remove(i);
        }
        let old = l.stop;
        l.stop = l
            .kinds
            .iter()
            .position(|&k| k == 0x00)
            .unwrap_or(l.kinds.len());
        let hidden: Vec<usize> = l.sets.range(l.stop..).map(|(&i, _)| i).collect();
        for i in hidden {
            l.remove(i);
        }
        again.extend(old..l.stop);
        let total = l.kinds.len();
        for i in again {
            if i >= l.stop || l.kinds[i] != 0x85 {
                continue;
            }
            let sc = usize::from(self.read_entry(image, at(i))?[1]);
            if sc < 2 || i + sc >= total {
                return Err(bad(format!("bad entry set at byte {}", at(i))));
            }
            let mut set = Vec::with_capacity((1 + sc) * ENTRY);
            for k in i..=i + sc {
                set.extend_from_slice(&self.read_entry(image, at(k))?);
            }
            let node = parse_set(&set, |k| at(i + k), clusters[i / per])?;
            let up = self.upcased(&node.name);
            l.insert(i, node, up);
        }
        l.clusters = clusters;
        Ok(())
    }
}

/// The entry set `set` (File entry first, then its secondaries) as a [`Node`], checked:
/// layout and SetChecksum. `at`: the image byte of the set's entry `k`; `cluster`: the one
/// holding its File entry.
fn parse_set(set: &[u8], at: impl Fn(usize) -> u64, cluster: u32) -> io::Result<Node> {
    let sc = set.len() / ENTRY - 1;
    if le16(set, 2) != crate::set_checksum(set) {
        return Err(bad(format!(
            "entry set checksum mismatch at byte {}",
            at(0)
        )));
    }
    let s = &set[ENTRY..2 * ENTRY];
    let name_len = usize::from(s[3]);
    let name_entries = name_len.div_ceil(15);
    if s[0] != 0xC0 || name_len == 0 || 1 + name_entries > sc {
        return Err(bad(format!("bad stream extension at byte {}", at(1))));
    }
    let mut units = Vec::with_capacity(name_entries * 15);
    for e in set[2 * ENTRY..].chunks(ENTRY).take(name_entries) {
        if e[0] != 0xC1 {
            return Err(bad(format!(
                "bad file name entry in the set at byte {}",
                at(0)
            )));
        }
        units.extend((0..15).map(|j| le16(e, 2 + 2 * j)));
    }
    units.truncate(name_len);
    // After the names: benign secondaries (Vendor Extension 0xE0, Vendor Allocation 0xE1,
    // ...) are kept as they are; an unknown critical one makes the set unreadable.
    let mut vendor_allocations = Vec::new();
    for e in set[(2 + name_entries) * ENTRY..].chunks(ENTRY) {
        if e[0] < 0xC0 || e[0] & 0x20 == 0 {
            return Err(bad(format!(
                "unknown secondary entry 0x{:02X} in the set at byte {}",
                e[0],
                at(0)
            )));
        }
        if e[1] & 1 != 0 && le64(e, 24) > 0 {
            vendor_allocations.push(Stream {
                first_cluster: le32(e, 20),
                len: le64(e, 24),
                no_fat_chain: e[1] & NO_FAT_CHAIN != 0,
            });
        }
    }
    let vendor_entries = sc > 1 + name_entries;
    let name = String::from_utf16(&units)
        .map_err(|_| bad(format!("name at byte {} is not valid UTF-16", at(0))))?;
    Ok(Node {
        name,
        attributes: le16(set, 4),
        stream: Stream {
            first_cluster: le32(s, 20),
            len: le64(s, 24),
            no_fat_chain: s[1] & NO_FAT_CHAIN != 0,
        },
        valid_len: le64(s, 8),
        cluster,
        entries: (0..=sc).map(at).collect(),
        vendor_entries,
        vendor_allocations,
    })
}

impl Volume {
    fn sector_size(&self) -> u64 {
        1 << self.sector_shift
    }

    /// The (planned) sector holding image byte `at`, copied on first touch, and `at`'s index in
    /// it; inside a cluster this plan allocated, that cluster's content instead.
    fn dir_sector(&mut self, image: &mut File, at: u64) -> io::Result<(&mut [u8], usize)> {
        let ss = self.sector_size();
        let start = at & !(ss - 1);
        if at >= self.heap_offset {
            let c = ((at - self.heap_offset) / self.cluster_size()) as u32 + FIRST_CLUSTER as u32;
            let base = self.cluster_offset(c);
            // The listing holding this entry re-reads it before its next use.
            let i = ((at - base) / ENTRY as u64) as usize;
            let per = (self.cluster_size() / ENTRY as u64) as usize;
            let held = self.changes.listings.get_mut();
            if let Some(&(first, k)) = held.owner.get(&c)
                && let Some(l) = held.by_first.get_mut(&first)
            {
                l.dirty.insert(k * per + i);
            }
            if self.changes.fresh.contains_key(&c) {
                let fresh = self.changes.fresh.get_mut(&c).unwrap();
                return Ok((fresh.as_mut_slice(), (at - base) as usize));
            }
        }
        if !self.changes.dir.contains_key(&start) {
            self.fits(ss)?;
        }
        let sector = match self.changes.dir.entry(start) {
            std::collections::btree_map::Entry::Occupied(e) => e.into_mut(),
            std::collections::btree_map::Entry::Vacant(e) => {
                let mut b = vec![0u8; ss as usize];
                read_at(image, start, &mut b)?;
                e.insert(b)
            }
        };
        Ok((sector.as_mut_slice(), (at - start) as usize))
    }

    fn set_fat(&mut self, c: u32, next: u32) -> io::Result<()> {
        let sector = (u64::from(c) * 4) >> self.sector_shift;
        if !self.changes.fat.contains(&sector) {
            self.fits(1 << self.sector_shift)?;
            self.changes.fat.insert(sector);
        }
        self.fat[c as usize] = next;
        Ok(())
    }

    fn set_free(&mut self, c: u32) -> io::Result<()> {
        if self.changes.before.is_none() {
            self.fits(self.bitmap.len() as u64)?;
            self.changes.before = Some(self.bitmap.clone());
        }
        // Never allocated again in this plan, even when this plan allocated it.
        let i = (c - FIRST_CLUSTER as u32) as usize;
        if let Some(b) = self.changes.before.as_mut().and_then(|b| b.get_mut(i / 8)) {
            *b |= 1 << (i % 8);
        }
        self.set_bit(c, false)
    }

    fn set_bit(&mut self, c: u32, used: bool) -> io::Result<()> {
        let i = (c - FIRST_CLUSTER as u32) as usize;
        let sector = (i as u64 / 8) >> self.sector_shift;
        if !self.changes.bitmap.contains(&sector) {
            self.fits(1 << self.sector_shift)?;
            self.changes.bitmap.insert(sector);
        }
        if used {
            self.bitmap[i / 8] |= 1 << (i % 8);
        } else {
            self.bitmap[i / 8] &= !(1 << (i % 8));
        }
        Ok(())
    }

    /// Whether cluster `c` is free now and was before this plan freed anything.
    fn allocatable(&self, c: u32) -> bool {
        let i = (c - FIRST_CLUSTER as u32) as usize;
        let was = self
            .changes
            .before
            .as_ref()
            .map_or(0, |b| b[i / 8] & (1 << (i % 8)));
        was == 0 && !self.in_use(c)
    }

    /// Clusters [`Self::allocatable`] holds: those free at open that this plan has not taken
    /// (what it frees is never allocatable).
    fn allocatable_clusters(&self) -> u64 {
        self.changes.free.saturating_sub(self.changes.taken)
    }

    /// The first run of `n` allocatable clusters in `from..to`.
    fn run_of(&self, from: u32, to: u32, n: u64) -> Option<u32> {
        let (mut start, mut len) = (from, 0);
        for c in from..to {
            if !self.allocatable(c) {
                len = 0;
                continue;
            }
            if len == 0 {
                start = c;
            }
            len += 1;
            if len == n {
                return Some(start);
            }
        }
        None
    }

    /// A run of `n` allocatable clusters from the cursor on, else from the heap's start.
    // ponytail: linear bitmap scans, a couple per file or folder (next fit keeps them short
    // while the free space is in one piece); a free-extent index would lift a huge, fragmented
    // image's many small files.
    fn find_run(&self, n: u64) -> Option<u32> {
        let (first, end) = (
            FIRST_CLUSTER as u32,
            self.cluster_count + FIRST_CLUSTER as u32,
        );
        let cursor = self.changes.cursor.clamp(first, end);
        self.run_of(cursor, end, n)
            .or_else(|| self.run_of(first, end, n))
    }

    /// Marks cluster `c` in use for this plan (its FAT entry is the caller's).
    fn take(&mut self, c: u32) -> io::Result<()> {
        self.set_bit(c, true)?;
        self.changes.taken += 1;
        self.changes.cursor = c.saturating_add(1);
        Ok(())
    }

    /// A new zeroed directory cluster, `prefer` when it is allocatable, else the next free one;
    /// marked in use (its FAT entry is the caller's). `Ok(None)`: the image is full.
    fn allocate(&mut self, prefer: Option<u32>) -> io::Result<Option<u32>> {
        let found = prefer
            .filter(|&c| self.valid(c) && self.allocatable(c))
            .or_else(|| self.find_run(1));
        let Some(c) = found else { return Ok(None) };
        let cs = self.cluster_size();
        self.fits(cs)?;
        self.changes.listings.get_mut().drop_cluster(c);
        self.changes.fresh.insert(c, vec![0u8; cs as usize]);
        self.take(c)?;
        Ok(Some(c))
    }

    /// Frees the clusters of `s`: bitmap bits, and its FAT entries unless NoFatChain.
    fn free_stream(&mut self, s: Stream) -> io::Result<()> {
        for c in self.chain(s)? {
            self.set_free(c)?;
            if !s.no_fat_chain {
                self.set_fat(c, 0)?;
            }
        }
        Ok(())
    }

    /// Deletes the file or folder at `path`: its entry set's InUse bits cleared, its clusters
    /// (and its Vendor Allocations') freed; for a folder, everything under it freed too (the
    /// sets inside are left as they are: the clusters holding them are free).
    /// `Ok(Err(finding))` when the op does not apply.
    fn delete(&mut self, image: &mut File, path: &str) -> io::Result<Result<(), String>> {
        let Some(node) = self.lookup(image, path)? else {
            return Ok(Err(format!("{path}: not found")));
        };
        self.clear_set(image, &node)?;
        // Each folder is listed before its own clusters are freed (a FAT-chained one's chain
        // is zeroed with them). Files are freed as they are listed; folders waiting to be
        // listed are kept on a stack (not recursed), charged to the budget with the listing.
        let held = node_bytes(&node);
        self.fits(held)?;
        self.changes.held = held;
        let mut stack = vec![node];
        while let Some(n) = stack.pop() {
            self.changes.held -= node_bytes(&n).min(self.changes.held);
            if n.is_dir() {
                let clusters = self.dir_chain(n.stream)?;
                let listing = self.read_dir(image, &clusters)?;
                for child in listing {
                    if child.is_dir() {
                        let b = node_bytes(&child);
                        self.fits(b)?;
                        self.changes.held += b;
                        stack.push(child);
                    } else {
                        self.free_node(&child)?;
                    }
                }
            }
            self.free_node(&n)?;
        }
        self.changes.held = 0;
        Ok(Ok(()))
    }

    /// Frees the clusters of `n`'s stream and of its Vendor Allocations.
    fn free_node(&mut self, n: &Node) -> io::Result<()> {
        self.free_stream(n.stream)?;
        for &s in &n.vendor_allocations {
            self.free_stream(s)?;
        }
        Ok(())
    }

    /// The 32 bytes of the (planned) directory entry at image byte `at`.
    fn read_entry(&self, image: &mut File, at: u64) -> io::Result<[u8; ENTRY]> {
        let mut e = [0u8; ENTRY];
        let ss = self.sector_size();
        let c = ((at - self.heap_offset) / self.cluster_size()) as u32 + FIRST_CLUSTER as u32;
        if let Some(fresh) = self.changes.fresh.get(&c) {
            let i = (at - self.cluster_offset(c)) as usize;
            e.copy_from_slice(&fresh[i..i + ENTRY]);
        } else if let Some(sector) = self.changes.dir.get(&(at & !(ss - 1))) {
            let i = (at & (ss - 1)) as usize;
            e.copy_from_slice(&sector[i..i + ENTRY]);
        } else {
            read_at(image, at, &mut e)?;
        }
        Ok(e)
    }

    /// Plans `e` as the directory entry at image byte `at` (an entry never spans sectors).
    fn write_entry(&mut self, image: &mut File, at: u64, e: &[u8]) -> io::Result<()> {
        let (sector, i) = self.dir_sector(image, at)?;
        sector[i..i + ENTRY].copy_from_slice(e);
        Ok(())
    }

    /// `node`'s whole entry set as planned so far.
    fn read_set(&self, image: &mut File, node: &Node) -> io::Result<Vec<u8>> {
        let mut set = Vec::with_capacity(node.entries.len() * ENTRY);
        for &at in &node.entries {
            set.extend_from_slice(&self.read_entry(image, at)?);
        }
        Ok(set)
    }

    /// `node`'s File and Stream Extension entries (attributes, times, data) with new File Name
    /// entries for `name`: NameLength, NameHash (this volume's up-case table), SecondaryCount
    /// and SetChecksum renewed. The set must hold no vendor entries.
    fn renamed_set(&self, image: &mut File, node: &Node, name: &str) -> io::Result<Vec<u8>> {
        let old = self.read_set(image, node)?;
        let head = old
            .get(..2 * ENTRY)
            .ok_or_else(|| bad(format!("{}: its entry set is short", node.name)))?;
        Ok(self.named_set(head, name))
    }

    /// File and Stream Extension entries `head` with File Name entries for `name`:
    /// NameLength, NameHash (this volume's up-case table), SecondaryCount and SetChecksum set.
    fn named_set(&self, head: &[u8], name: &str) -> Vec<u8> {
        let units: Vec<u16> = name.encode_utf16().collect();
        let name_entries = units.len().div_ceil(15);
        let mut set = vec![0u8; ENTRY * (2 + name_entries)];
        set[..2 * ENTRY].copy_from_slice(head);
        set[1] = (1 + name_entries) as u8;
        set[ENTRY + 3] = units.len() as u8;
        let hash = names::name_hash(&self.upcased(name));
        set[ENTRY + 4..ENTRY + 6].copy_from_slice(&hash.to_le_bytes());
        for (i, chunk) in units.chunks(15).enumerate() {
            let e = &mut set[(2 + i) * ENTRY..(3 + i) * ENTRY];
            e[0] = 0xC1;
            for (j, u) in chunk.iter().enumerate() {
                e[2 + 2 * j..4 + 2 * j].copy_from_slice(&u.to_le_bytes());
            }
        }
        let sum = crate::set_checksum(&set);
        set[2..4].copy_from_slice(&sum.to_le_bytes());
        set
    }

    /// A new entry set as the writer makes one (its fixed timestamps): a file (archive) or a
    /// folder named `name`, its data in `s` (ValidDataLength = DataLength).
    fn new_set(&self, name: &str, dir: bool, s: Stream) -> Vec<u8> {
        let mut head = [0u8; 2 * ENTRY];
        head[0] = 0x85;
        let attr = if dir { ATTR_DIRECTORY } else { ATTR_ARCHIVE };
        head[4..6].copy_from_slice(&attr.to_le_bytes());
        for at in [8, 12, 16] {
            head[at..at + 4].copy_from_slice(&TIMESTAMP.to_le_bytes()); // created, modified, accessed
        }
        set_stream(&mut head[ENTRY..], s);
        self.named_set(&head, name)
    }

    /// Clears the InUse bit of every entry of `node`'s set.
    fn clear_set(&mut self, image: &mut File, node: &Node) -> io::Result<()> {
        for &at in &node.entries {
            let (sector, i) = self.dir_sector(image, at)?;
            sector[i] &= 0x7F;
        }
        Ok(())
    }
}

/// What a [`Node`] held in a list costs: its slot twice (a `Vec` grows by doubling), its
/// name and its two lists.
fn node_bytes(n: &Node) -> u64 {
    (2 * std::mem::size_of::<Node>()
        + n.name.capacity()
        + n.entries.capacity() * 8
        + n.vendor_allocations.capacity() * std::mem::size_of::<Stream>()) as u64
}

/// Stream Extension entry `e`'s flags (AllocationPossible, NoFatChain), lengths and first
/// cluster set to `s` (ValidDataLength = DataLength).
fn set_stream(e: &mut [u8], s: Stream) {
    e[0] = 0xC0;
    e[1] = if s.no_fat_chain && s.first_cluster != 0 {
        ALLOCATION_POSSIBLE | NO_FAT_CHAIN
    } else {
        ALLOCATION_POSSIBLE
    };
    e[8..16].copy_from_slice(&s.len.to_le_bytes());
    e[20..24].copy_from_slice(&s.first_cluster.to_le_bytes());
    e[24..32].copy_from_slice(&s.len.to_le_bytes());
}

/// `path`'s parent folder (`""`: the root) and last component.
fn split(path: &str) -> (&str, &str) {
    path.rsplit_once('/').unwrap_or(("", path))
}

impl Volume {
    /// Renames or moves `from` to `to`: in place when the new set has as many entries and
    /// stays in its folder, else the old set is marked unused and the new one placed in the
    /// target folder ([`Self::place`]). `Ok(Err(finding))` when the op does not apply.
    fn rename(&mut self, image: &mut File, from: &str, to: &str) -> io::Result<Result<(), String>> {
        let Some(node) = self.lookup(image, from)? else {
            return Ok(Err(format!("{from}: not found")));
        };
        let (parent, name) = split(to);
        if let Some(why) = names::problem(name) {
            return Ok(Err(format!("{to}: {why}")));
        }
        if node.vendor_entries {
            return Ok(Err(format!(
                "{from}: its exFAT entry set holds vendor entries, so it can't be renamed in place"
            )));
        }
        let up = |p: &str| -> Vec<Vec<u16>> { p.split('/').map(|c| self.upcased(c)).collect() };
        let (f, t) = (up(from), up(to));
        if node.is_dir() && t.len() > f.len() && t[..f.len()] == f[..] {
            return Ok(Err(format!("{to}: a folder can't be moved into itself")));
        }
        if let Some(n) = self.lookup(image, to)?
            && n.entries[0] != node.entries[0]
        {
            return Ok(Err(format!("{to}: already exists")));
        }
        let target = if parent.is_empty() {
            None
        } else {
            match self.lookup(image, parent)? {
                Some(n) if n.is_dir() => Some(n),
                _ => return Ok(Err(format!("{to}: folder {parent} not found"))),
            }
        };
        let set = self.renamed_set(image, &node, name)?;
        if f[..f.len() - 1] == t[..t.len() - 1] && set.len() == node.entries.len() * ENTRY {
            for (e, &at) in set.chunks(ENTRY).zip(&node.entries) {
                self.write_entry(image, at, e)?;
            }
            return Ok(Ok(()));
        }
        self.clear_set(image, &node)?;
        self.place(image, target, &set, to)
    }

    /// Plans `set` as the entries from index `start` of the folder held by `clusters`.
    fn write_set(
        &mut self,
        image: &mut File,
        clusters: &[u32],
        per: usize,
        start: usize,
        set: &[u8],
    ) -> io::Result<()> {
        for (k, e) in set.chunks(ENTRY).enumerate() {
            let j = start + k;
            let &c = clusters
                .get(j / per)
                .ok_or_else(|| bad("an entry set past its folder's end"))?;
            self.write_entry(image, self.cluster_offset(c) + (j % per * ENTRY) as u64, e)?;
        }
        Ok(())
    }

    /// Writes `set` into the first run of unused entries of the folder `dir` (`None`: the
    /// root) that leaves an end-of-directory entry after the last set, else appends as many
    /// clusters as that takes and writes the set from the start of the unused entries the
    /// folder ends with (nothing may follow the end-of-directory entries, so the set spans
    /// into the new clusters): the chain extended (a NoFatChain folder stays one when the next
    /// clusters are free, else gets a FAT chain) and its DataLength and ValidDataLength grown.
    /// Without room for those clusters the set is left out, counted in `Changes::short`.
    fn place(
        &mut self,
        image: &mut File,
        dir: Option<Node>,
        set: &[u8],
        to: &str,
    ) -> io::Result<Result<(), String>> {
        let clusters = match &dir {
            None => self.whole_chain(self.root_cluster)?,
            Some(n) => self.dir_chain(n.stream)?,
        };
        let Some(&last) = clusters.last() else {
            return Err(bad(format!("{to}: its folder owns no cluster")));
        };
        let cs = self.cluster_size();
        let kinds = self.with_listing(image, &clusters, |l| l.kinds.clone())?;
        let per = cs as usize / ENTRY;
        let need = set.len() / ENTRY;
        let total = kinds.len();
        // Readers stop at the first end-of-directory (0x00) entry, and one must stay after the
        // last set (a reader may otherwise run past the folder's end). `end`: where the
        // unused entries leading up to it start. A set goes into a run of deleted entries
        // before `end`, else from `end` on with an end-of-directory entry after it.
        let stop = kinds.iter().position(|&k| k == 0x00).unwrap_or(total);
        let visible = kinds.get(..stop).unwrap_or_default();
        let end = stop - visible.iter().rev().take_while(|&&k| k & 0x80 == 0).count();
        let mut run = 0;
        for (i, &k) in visible.iter().enumerate() {
            run = if k & 0x80 == 0 { run + 1 } else { 0 };
            if run >= need && i < end {
                self.write_set(image, &clusters, per, i + 1 - need, set)?;
                return Ok(Ok(()));
            }
        }
        let after = end + need;
        if after < total {
            self.write_set(image, &clusters, per, end, set)?;
            // Every entry after an end-of-directory one must be one too: the deleted entries
            // the set did not cover are zeroed (and the entry after it, when not one yet).
            for j in after..stop.max(after + 1) {
                if kinds.get(j).is_some_and(|&k| k != 0x00) {
                    self.write_set(image, &clusters, per, j, &[0u8; ENTRY])?;
                }
            }
            return Ok(Ok(()));
        }
        // Appended: the set from `end` on, spanning into new clusters (zeroed, so the entry
        // after it is an end-of-directory one).
        let more = (after + 1 - total).div_ceil(per) as u64;
        let len = clusters.len() as u64 * cs;
        if len + more * cs > MAX_DIR_BYTES {
            return Ok(Err(format!(
                "{to}: its folder is full (exFAT holds 256 MiB of entries)"
            )));
        }
        if self.allocatable_clusters() < more {
            // No room: counted for the plan's one finding, the set left out.
            self.changes.short += more;
            return Ok(Ok(()));
        }
        let mut clusters = clusters;
        for _ in 0..more {
            let prev = clusters.last().copied().unwrap_or(last);
            let c = self
                .allocate(prev.checked_add(1))?
                .ok_or_else(|| bad("no free cluster after counting one"))?;
            clusters.push(c);
        }
        self.write_set(image, &clusters, per, end, set)?;
        // A NoFatChain folder whose new clusters follow its run stays one; any other gets
        // (or keeps) a FAT chain, written from the first link that is new.
        let old = clusters.len() - more as usize;
        let no_fat_chain = dir.as_ref().is_some_and(|n| n.stream.no_fat_chain);
        let contiguous = no_fat_chain && clusters.windows(2).all(|w| w[1] == w[0] + 1);
        if !contiguous {
            let from = if no_fat_chain {
                0
            } else {
                old.saturating_sub(1)
            };
            for w in clusters.get(from..).unwrap_or_default().windows(2) {
                if let [a, b] = *w {
                    self.set_fat(a, b)?;
                }
            }
            if let Some(&tail) = clusters.last() {
                self.set_fat(tail, FAT_EOC)?;
            }
        }
        let Some(n) = dir else { return Ok(Ok(())) };
        let (Some(&first), Some(&second)) = (n.entries.first(), n.entries.get(1)) else {
            return Err(bad(format!("{to}: its folder's entry set is short")));
        };
        let mut owner = self.read_set(image, &n)?;
        let grown = more * cs;
        let s = owner
            .get_mut(ENTRY..2 * ENTRY)
            .ok_or_else(|| bad(format!("{to}: its folder's entry set is short")))?;
        if !contiguous {
            s[1] &= !NO_FAT_CHAIN;
        }
        s[8..16].copy_from_slice(&(n.valid_len + grown).to_le_bytes());
        s[24..32].copy_from_slice(&(len + grown).to_le_bytes());
        let sum = crate::set_checksum(&owner);
        owner[2..4].copy_from_slice(&sum.to_le_bytes());
        self.write_entry(image, first, &owner[..ENTRY])?;
        self.write_entry(image, second, &owner[ENTRY..2 * ENTRY])?;
        Ok(Ok(()))
    }
}

/// Where a new entry set goes.
enum Parent {
    Root,
    Dir(Node),
    /// Not created for want of room (counted in `Changes::short`): the op goes no further.
    Short,
}

impl Volume {
    /// `n` (≥ 1) allocatable clusters as runs `(first, count)`: one contiguous run when there
    /// is one, else the lowest ones. Taken (marked in use, counted) and FAT-chained in order,
    /// a contiguous run too (as the writer does). `Ok(None)`: fewer than `n` are allocatable.
    fn take_clusters(&mut self, n: u64) -> io::Result<Option<Vec<(u32, u64)>>> {
        if n > self.allocatable_clusters() {
            return Ok(None);
        }
        let runs = match self.find_run(n) {
            Some(c) => vec![(c, n)],
            None => self.lowest_runs(n)?,
        };
        let mut prev: Option<u32> = None;
        for &(first, count) in &runs {
            for c in (first..).take(count as usize) {
                self.take(c)?;
                if let Some(p) = prev {
                    self.set_fat(p, c)?;
                }
                prev = Some(c);
            }
        }
        if let Some(p) = prev {
            self.set_fat(p, FAT_EOC)?;
        }
        Ok(Some(runs))
    }

    /// The lowest `n` allocatable clusters as runs, the list charged to the budget.
    fn lowest_runs(&self, n: u64) -> io::Result<Vec<(u32, u64)>> {
        let mut runs: Vec<(u32, u64)> = Vec::new();
        let mut got = 0;
        for c in FIRST_CLUSTER as u32..self.cluster_count + FIRST_CLUSTER as u32 {
            if got == n {
                break;
            }
            if !self.allocatable(c) {
                continue;
            }
            got += 1;
            match runs.last_mut() {
                Some((first, count)) if u64::from(*first) + *count == u64::from(c) => *count += 1,
                _ => {
                    self.fits((runs.len() as u64 + 1) * 2 * 16)?;
                    runs.push((c, 1));
                }
            }
        }
        if got < n {
            return Err(bad("fewer free clusters than counted"));
        }
        Ok(runs)
    }

    /// Plans one free-space write, charged to the budget.
    fn push_data(&mut self, offset: u64, data: Data) -> io::Result<()> {
        let held = match &data {
            Data::Bytes(b) => b.len() as u64,
            _ => 0,
        };
        self.fits(held + 2 * std::mem::size_of::<Write>() as u64)?;
        self.changes.data_bytes += held;
        self.changes.data.push(Write {
            offset,
            data,
            live: false,
        });
        Ok(())
    }
}

/// `len` bytes of `data` from `offset` (within it).
fn slice(data: &Data, offset: u64, len: u64) -> io::Result<Data> {
    Ok(match data {
        Data::Zero(_) => Data::Zero(len),
        Data::Source { id, offset: at, .. } => Data::Source {
            id: *id,
            offset: at + offset,
            len,
        },
        Data::Bytes(b) => {
            let range = usize::try_from(offset)
                .ok()
                .zip(usize::try_from(offset + len).ok());
            let part = range.and_then(|(a, z)| b.get(a..z));
            Data::Bytes(part.ok_or_else(|| bad("data slice out of range"))?.to_vec())
        }
    })
}

impl Volume {
    /// The folder at game path `dir` (`""`: the root), made with its parents when missing:
    /// each new one a zeroed cluster (NoFatChain) and an entry set in its parent.
    /// `Ok(Err(finding))`: a part of the path is a file or a name exFAT can't hold.
    fn ensure_dir(&mut self, image: &mut File, dir: &str) -> io::Result<Result<Parent, String>> {
        let mut parent = Parent::Root;
        if dir.is_empty() {
            return Ok(Ok(parent));
        }
        let mut prefix = String::new();
        for (i, part) in dir.split('/').enumerate() {
            if !prefix.is_empty() {
                prefix.push('/');
            }
            prefix.push_str(part);
            match self.lookup(image, &prefix)? {
                Some(n) if n.is_dir() => parent = Parent::Dir(n),
                Some(_) => return Ok(Err(format!("{prefix} is a file"))),
                None => {
                    if let Some(why) = names::problem(part) {
                        return Ok(Err(format!("{prefix}: {why}")));
                    }
                    let key = self.upcased(&prefix);
                    if !self.changes.missing.contains(&key) {
                        let at = match parent {
                            Parent::Dir(n) => Some(n),
                            _ => None,
                        };
                        if let Err(f) = self.new_folder(image, at, part, &prefix)? {
                            return Ok(Err(f));
                        }
                        if let Some(n) = self.lookup(image, &prefix)? {
                            parent = Parent::Dir(n);
                            continue;
                        }
                        self.changes.missing.insert(key);
                    }
                    // Not made for want of room (by this op or an earlier one): each folder
                    // below it would take a cluster too, counted once.
                    for rest in dir.split('/').skip(i + 1) {
                        prefix.push('/');
                        prefix.push_str(rest);
                        let key = self.upcased(&prefix);
                        if self.changes.missing.insert(key) {
                            self.changes.short += 1;
                        }
                    }
                    return Ok(Ok(Parent::Short));
                }
            }
        }
        Ok(Ok(parent))
    }

    /// One new, empty folder `name` in `dir` (`None`: the root): a zeroed cluster and its set.
    /// No room: counted, nothing placed.
    fn new_folder(
        &mut self,
        image: &mut File,
        dir: Option<Node>,
        name: &str,
        path: &str,
    ) -> io::Result<Result<(), String>> {
        let Some(c) = self.allocate(None)? else {
            self.changes.short += 1;
            return Ok(Ok(()));
        };
        self.set_fat(c, FAT_EOC)?;
        let s = Stream {
            first_cluster: c,
            len: self.cluster_size(),
            no_fat_chain: true,
        };
        let set = self.new_set(name, true, s);
        self.place(image, dir, &set, path)
    }

    /// [`Op::Mkdir`]: the folder and its parents; an existing folder is left as it is.
    fn mkdir(&mut self, image: &mut File, path: &str) -> io::Result<Result<(), String>> {
        Ok(self.ensure_dir(image, path)?.map(|_| ()))
    }
}

impl Volume {
    /// [`Op::Put`]: the file at `path` (its folders made when missing) gets `len` bytes of
    /// `data` in new clusters (one run when there is one, NoFatChain; else a FAT chain), the
    /// last cluster's tail zeroed, all as free-space writes. A new file gets a new set; a
    /// replaced one keeps its set (name, attributes, timestamps) with the new stream, and its
    /// old clusters are freed after (its old data never overwritten). No room: counted.
    fn put(
        &mut self,
        image: &mut File,
        path: &str,
        len: u64,
        data: &Data,
    ) -> io::Result<Result<(), String>> {
        if data.len() != len {
            return Ok(Err(format!(
                "{path}: {len} bytes, but its data holds {}",
                data.len()
            )));
        }
        let (parent, name) = split(path);
        if let Some(why) = names::problem(name) {
            return Ok(Err(format!("{path}: {why}")));
        }
        let dir = match self.ensure_dir(image, parent)? {
            Err(f) => return Ok(Err(f)),
            Ok(Parent::Short) => {
                // Its folder can't be made: its data counts all the same.
                self.changes.short += len.div_ceil(self.cluster_size());
                return Ok(Ok(()));
            }
            Ok(Parent::Root) => None,
            Ok(Parent::Dir(n)) => Some(n),
        };
        let old = self.lookup(image, path)?;
        if old.as_ref().is_some_and(Node::is_dir) {
            return Ok(Err(format!("{path} is a folder")));
        }
        let s = self.put_data(len, data)?;
        match old {
            Some(n) if !n.vendor_entries => {
                let mut set = self.read_set(image, &n)?;
                let e = set
                    .get_mut(ENTRY..2 * ENTRY)
                    .ok_or_else(|| bad(format!("{path}: its entry set is short")))?;
                set_stream(e, s);
                let sum = crate::set_checksum(&set);
                set[2..4].copy_from_slice(&sum.to_le_bytes());
                for (e, &at) in set.chunks(ENTRY).zip(&n.entries) {
                    self.write_entry(image, at, e)?;
                }
                self.free_node(&n)?;
                Ok(Ok(()))
            }
            // Vendor entries can't be carried into a new stream: the set is replaced.
            Some(n) => {
                self.clear_set(image, &n)?;
                self.free_node(&n)?;
                let set = self.new_set(&n.name, false, s);
                self.place(image, dir, &set, path)
            }
            None => {
                let set = self.new_set(name, false, s);
                self.place(image, dir, &set, path)
            }
        }
    }

    /// New clusters for `len` bytes of `data`, its writes planned; the stream that holds
    /// them. No room: counted, and an empty stream stands in (the plan is refused anyway).
    fn put_data(&mut self, len: u64, data: &Data) -> io::Result<Stream> {
        let cs = self.cluster_size();
        let n = len.div_ceil(cs);
        let empty = Stream {
            first_cluster: 0,
            len: 0,
            no_fat_chain: false,
        };
        if n == 0 {
            return Ok(empty);
        }
        let Some(runs) = self.take_clusters(n)? else {
            self.changes.short += n;
            return Ok(empty);
        };
        let mut done = 0;
        for &(first, count) in &runs {
            let at = self.cluster_offset(first);
            let room = count * cs;
            let part = room.min(len - done);
            if part > 0 {
                self.push_data(at, slice(data, done, part)?)?;
            }
            if part < room {
                self.push_data(at + part, Data::Zero(room - part))?;
            }
            done += part;
        }
        Ok(Stream {
            first_cluster: runs.first().map_or(0, |r| r.0),
            len,
            no_fat_chain: runs.len() == 1,
        })
    }
}

impl Volume {
    /// The change set as writes: new directory clusters and file data as free-space writes,
    /// then the live ones, each changed sector once between two writes of the boot sector: the
    /// first sets VolumeDirty, the last clears it with PercentInUse updated (both outside the
    /// boot checksum). In between: directory sectors (entry sets marked
    /// deleted before the clusters they owned are freed), FAT, bitmap. No change, no writes.
    fn writes(mut self, image: &mut File) -> io::Result<Vec<Write>> {
        let ss = self.sector_size();
        let c = &self.changes;
        if c.dir.is_empty() && c.fat.is_empty() && c.bitmap.is_empty() {
            return Ok(Vec::new());
        }
        let mut boot = vec![0u8; ss as usize];
        read_at(image, 0, &mut boot)?;
        let mut first = boot.clone();
        first[106] |= 0x2;
        // New directory clusters first: free space (allocated only from clusters free before
        // this plan), referenced by the live writes after them.
        let mut out: Vec<Write> = std::mem::take(&mut self.changes.fresh)
            .into_iter()
            .map(|(c, b)| Write {
                offset: self.cluster_offset(c),
                data: Data::Bytes(b),
                live: false,
            })
            .collect();
        // New files' data and zeroed tails: free space too.
        out.append(&mut self.changes.data);
        out.push(Write {
            offset: 0,
            data: Data::Bytes(first),
            live: true,
        });
        let mut live = |offset: u64, b: Vec<u8>| {
            out.push(Write {
                offset,
                data: Data::Bytes(b),
                live: true,
            })
        };
        for (at, b) in std::mem::take(&mut self.changes.dir) {
            live(at, b);
        }
        for &i in &self.changes.fat {
            let at = self.fat_offset + i * ss;
            let mut b = vec![0u8; ss as usize];
            read_at(image, at, &mut b)?;
            let first = (i * ss / 4) as usize;
            let end = (first + (ss / 4) as usize).min(self.fat.len());
            for (j, e) in self.fat[first..end].iter().enumerate() {
                b[j * 4..j * 4 + 4].copy_from_slice(&e.to_le_bytes());
            }
            live(at, b);
        }
        let bitmap = self.chain(self.bitmap_stream)?;
        let cs = self.cluster_size();
        for &i in &self.changes.bitmap {
            let off = i * ss;
            let at = self.cluster_offset(bitmap[(off / cs) as usize]) + off % cs;
            let mut b = vec![0u8; ss as usize];
            read_at(image, at, &mut b)?;
            let end = (off + ss).min(self.bitmap.len() as u64);
            let n = (end - off) as usize;
            b[..n].copy_from_slice(&self.bitmap[off as usize..end as usize]);
            live(at, b);
        }
        if boot[112] != 0xFF {
            let cc = u64::from(self.cluster_count);
            boot[112] = ((cc - self.free_clusters()) * 100 / cc) as u8;
        }
        boot[106] &= !0x2;
        live(0, boot);
        Ok(out)
    }
}

/// Plans the writes that apply `ops` to the image, in order (each op sees the ones before
/// it), reading it only. An op that does not apply (a missing path, a taken name, a path
/// through a file) is a finding; so is too little room, once for the whole plan: "the changes
/// need N MB, the image has M MB free" (decimal MB; data and new directory clusters counted,
/// clusters the plan frees not reused; the image never grows).
pub fn plan(image: &mut File, ops: &[Op]) -> Result<Vec<Write>, Vec<String>> {
    plan_sized(image, ops).0
}

/// What a plan allocates in the image's free space and what it had: `needed` counts every
/// cluster it takes (file data, new and grown directories), those it lacked room for too;
/// `free` the free clusters when the volume was opened. Bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Space {
    pub needed: u64,
    pub free: u64,
}

/// [`plan`], with the [`Space`] the plan counted (`None` when the volume could not be read or
/// was not planned).
pub fn plan_sized(
    image: &mut File,
    ops: &[Op],
) -> (Result<Vec<Write>, Vec<String>>, Option<Space>) {
    match Volume::open(image) {
        Ok(v) => {
            let mut room = None;
            let planned = plan_volume(v, image, ops, &mut room);
            (planned, room)
        }
        Err(e) => (Err(vec![e.to_string()]), None),
    }
}

fn plan_volume(
    mut v: Volume,
    image: &mut File,
    ops: &[Op],
    room: &mut Option<Space>,
) -> Result<Vec<Write>, Vec<String>> {
    let fail = |e: io::Error| vec![e.to_string()];
    if v.dirty {
        return Err(vec![
            "the image was not unmounted cleanly (exFAT VolumeDirty): it can't be edited in \
             place; rebuild it instead"
                .into(),
        ]);
    }
    let mut findings = Vec::new();
    for op in ops {
        // Once the plan is short of room, later ops may fail only because of it (a folder or
        // file left out): their findings are dropped, the one no-room finding stands for them.
        let short = v.changes.short > 0;
        let done = match op {
            Op::Delete { path } => v.delete(image, path).map_err(fail)?,
            Op::Rename { from, to } => v.rename(image, from, to).map_err(fail)?,
            Op::Mkdir { path } => v.mkdir(image, path).map_err(fail)?,
            Op::Put { path, len, data } => v.put(image, path, *len, data).map_err(fail)?,
        };
        if let Err(f) = done
            && !short
        {
            findings.push(f);
        }
    }
    let c = &v.changes;
    let cs = v.cluster_size();
    *room = Some(Space {
        needed: (c.taken + c.short) * cs,
        free: c.free * cs,
    });
    if c.short > 0 {
        findings.push(format!(
            "the changes need {} MB, the image has {} MB free",
            ((c.taken + c.short) * cs).div_ceil(1_000_000),
            c.free * cs / 1_000_000
        ));
    }
    if !findings.is_empty() {
        return Err(findings);
    }
    v.writes(image).map_err(fail)
}
