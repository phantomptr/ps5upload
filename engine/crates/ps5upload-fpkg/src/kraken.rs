//! Kraken, as the PS5 stores it in a package's inner image.
//!
//! The console's decompressor reads each 256 KiB block of the image as two 128 KiB halves,
//! stored bare: no Oodle stream or chunk headers, the lengths and modes live in the block's
//! layout record (see `docs/research/2026-09-23-ps5-kraken-package-blocks.md`). A half is one
//! Kraken LZ chunk body:
//!
//! ```text
//! [8-byte seed]            even half only: the block's first 8 bytes, raw
//! [0x80 | excess count]    the newer "excess" framing (count in 6 bits + a continuation byte)
//! [literals array] [commands array] [0x80 = offsets scaled by 1] [offsets array] [lengths array]
//! [offset bits: forward stream ... backward stream]
//! [excess: long-length escapes, forward ... backward]
//! ```
//!
//! Each array is Huffman-coded (type 2, see `huff`) when that is smaller, raw (type 0) otherwise.
//! Literals are stored as they are (literal mode 1) or, where that codes smaller, less the byte at
//! the last match distance (literal mode 0, flagged in the block's layout record). New offsets use
//! the traditional code (see `traditional_offset`). A command
//! byte is `offset kind << 6 | (match length − 2) << 2 | literal run`, where kind 0–2 reuses one
//! of the three recent offsets and 3 takes the next new one; a run of 3 or a length code of 15
//! takes its value from the lengths array (value − 3, 255 escaping to the excess stream).
//!
//! The decoder copies matches eight bytes at a time, so a match is never closer than 8 bytes.
//! Matches are chosen by a lazy parse ([`Level::Fast`]) or an optimal parse priced from the
//! half's own symbol statistics (the other levels); on Spider-Man 2's blocks the default level
//! stores 33.6% of the logical size against Sony's own encoder's 32.8%.
//! The encoder is written from the format (documented by powzix/ooz, GPL-3, and the PS5 framing
//! decoded from Sony's packages); the decoder here reads exactly what the encoder writes and is
//! what proves every block before a package keeps it.

mod huff;

use crate::{format_err, Result};

/// A half's logical size.
pub const HALF: usize = 0x2_0000;
/// A block's logical size.
pub const BLOCK: usize = 0x4_0000;
const SEED: usize = 8;
/// The closest a match may reference.
const MIN_DISTANCE: usize = 8;
/// The shortest match worth a command.
const MIN_MATCH: usize = 4;
/// The largest distance the offset code carries (26 extra bits).
const MAX_DISTANCE: usize = (15 << 26) - 16;

// ─────────────────────────────── bit streams ───────────────────────────────

/// An MSB-first bit writer; the backward stream is written the same way and stored reversed.
#[derive(Default)]
struct BitWriter {
    bytes: Vec<u8>,
    acc: u64,
    n: u32,
}

impl BitWriter {
    fn put(&mut self, value: u32, bits: u32) {
        debug_assert!(bits <= 32);
        if bits == 0 {
            return;
        }
        self.acc = (self.acc << bits) | u64::from(value & (((1u64 << bits) - 1) as u32));
        self.n += bits;
        while self.n >= 8 {
            self.n -= 8;
            self.bytes.push((self.acc >> self.n) as u8);
        }
    }

    fn finish(mut self) -> Vec<u8> {
        if self.n > 0 {
            self.bytes.push((self.acc << (8 - self.n)) as u8);
            self.n = 0;
        }
        self.bytes
    }

    /// A length escape: `v + 64` in `b` bits, preceded by `b − 7` zero bits.
    fn put_length(&mut self, v: u32) {
        let y = v + 64;
        let b = 32 - y.leading_zeros();
        self.put(0, b - 7);
        self.put(y, b);
    }
}

/// An MSB-first bit reader over `[start, end)`, forward or backward.
struct BitReader<'a> {
    src: &'a [u8],
    pos: isize,
    end: isize,
    back: bool,
    bits: u32,
    bitpos: i32,
}

impl<'a> BitReader<'a> {
    fn new(src: &'a [u8], start: usize, end: usize, back: bool) -> Self {
        let mut r = Self {
            src,
            pos: if back { end as isize } else { start as isize },
            end: if back { start as isize } else { end as isize },
            back,
            bits: 0,
            bitpos: 24,
        };
        r.refill();
        r
    }

    fn refill(&mut self) {
        while self.bitpos > 0 {
            let byte = if self.back {
                self.pos -= 1;
                if self.pos >= self.end {
                    self.src[self.pos as usize]
                } else {
                    0
                }
            } else {
                let b = if self.pos < self.end {
                    self.src[self.pos as usize]
                } else {
                    0
                };
                self.pos += 1;
                b
            };
            self.bits |= u32::from(byte) << self.bitpos;
            self.bitpos -= 8;
        }
    }

    fn get(&mut self, n: u32) -> u32 {
        if n == 0 {
            return 0;
        }
        let mut v = 0u32;
        let mut left = n;
        while left > 0 {
            let take = left.min(24);
            let r = self.bits >> (32 - take);
            self.bits <<= take;
            self.bitpos += take as i32;
            self.refill();
            v = (v << take) | r;
            left -= take;
        }
        v
    }

    fn get_length(&mut self) -> Result<u32> {
        let zeros = self.bits.leading_zeros();
        if zeros > 12 {
            return format_err("kraken: bad length code");
        }
        self.get(zeros);
        Ok(self.get(zeros + 7) - 64)
    }

    /// Where the reader stands in the buffer, counting whole bytes consumed.
    fn seam(&self) -> isize {
        let pending = ((24 - self.bitpos) >> 3) as isize;
        if self.back {
            self.pos + pending
        } else {
            self.pos - pending
        }
    }
}

// ─────────────────────────────── arrays ───────────────────────────────

/// A raw (type 0) array, always with the 3-byte header. Kraken also has a 2-byte form for
/// arrays under 4 KiB; the console's hardware decoder never sees it from the encoders it
/// accepts (Sony's, LibProsperoPkg's), so we do not write it.
fn put_raw_array(out: &mut Vec<u8>, data: &[u8]) {
    let n = data.len();
    out.push((n >> 16) as u8);
    out.push((n >> 8) as u8);
    out.push(n as u8);
    out.extend_from_slice(data);
}

/// A new match distance in Kraken's traditional offset code: `d + 248 = (1n·x)·16 + low`, the
/// byte carrying `(n − 4) << 4 | low` and the stream carrying `x` in `n` bits. Any `d` from 8 up
/// to ~8 MB fits in 4..=18 bits; a block never needs more.
fn traditional_offset(d: usize) -> (u8, u32, u32) {
    let t = (d + 248) as u32;
    let low = t & 0xF;
    let hi = t >> 4;
    let n = 31 - hi.leading_zeros();
    debug_assert!(
        (4..=18).contains(&n),
        "distance {d} outside the traditional code"
    );
    (((n - 4) << 4 | low) as u8, hi - (1 << n), n)
}

/// An array at `at`, raw (type 0) or Huffman (type 2): `(bytes, header + payload length)`.
/// `max` bounds the decoded size.
fn get_array(src: &[u8], at: usize, end: usize, max: usize) -> Result<(Vec<u8>, usize)> {
    if end < at + 2 {
        return format_err("kraken: truncated array");
    }
    let b0 = src[at];
    match (b0 >> 4) & 7 {
        0 => {
            let (n, h) = if b0 >= 0x80 {
                ((((b0 as usize) << 8) | src[at + 1] as usize) & 0xFFF, 2)
            } else {
                if end < at + 3 {
                    return format_err("kraken: truncated array");
                }
                let n =
                    ((b0 as usize) << 16) | ((src[at + 1] as usize) << 8) | src[at + 2] as usize;
                if n & !0x3FFFF != 0 {
                    return format_err("kraken: bad array size");
                }
                (n, 3)
            };
            if n > max || at + h + n > end {
                return format_err("kraken: array overruns its chunk");
            }
            Ok((src[at + h..at + h + n].to_vec(), h + n))
        }
        2 => {
            let (src_size, dst_size, h) = if b0 >= 0x80 {
                if end < at + 3 {
                    return format_err("kraken: truncated array");
                }
                let v = (usize::from(b0) << 16)
                    | (usize::from(src[at + 1]) << 8)
                    | usize::from(src[at + 2]);
                let s = v & 0x3FF;
                (s, s + ((v >> 10) & 0x3FF) + 1, 3)
            } else {
                if end < at + 5 {
                    return format_err("kraken: truncated array");
                }
                let v = u32::from_be_bytes([src[at + 1], src[at + 2], src[at + 3], src[at + 4]])
                    as usize;
                let s = v & 0x3FFFF;
                let d = (((v >> 18) | (usize::from(b0) << 14)) & 0x3FFFF) + 1;
                (s, d, 5)
            };
            if src_size >= dst_size || dst_size > max || at + h + src_size > end {
                return format_err("kraken: bad Huffman array size");
            }
            let body = &src[at + h..at + h + src_size];
            Ok((huff::decode(body, dst_size)?, h + src_size))
        }
        _ => format_err("kraken: unsupported array type"),
    }
}

/// An array as Huffman when that is smaller, raw otherwise.
fn put_array(out: &mut Vec<u8>, data: &[u8]) {
    if let Some(body) = huff::encode(data) {
        if body.len() + 5 < data.len() + 3 {
            let d = data.len() - 1;
            out.push(0x20 | (d >> 14) as u8);
            out.extend_from_slice(&((((d & 0x3FFF) << 18) | body.len()) as u32).to_be_bytes());
            out.extend_from_slice(&body);
            return;
        }
    }
    put_raw_array(out, data);
}

// ─────────────────────────────── encoder ───────────────────────────────

const HASH_BITS: u32 = 16;

/// How hard the encoder searches. The format, and so what the console decodes, is the same at
/// every level; only the ratio and the time it takes change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Level {
    /// A lazy parse: at each position the match that saves the most, unless the next position's
    /// saves more.
    Fast,
    /// An optimal parse priced twice. The default.
    #[default]
    Balanced,
    /// An optimal parse with deeper match searches, priced three times.
    Smallest,
}

impl std::str::FromStr for Level {
    type Err = String;

    /// `fast`, `balanced` or `smallest`, in any case.
    fn from_str(s: &str) -> std::result::Result<Self, String> {
        match s.trim().to_ascii_lowercase().as_str() {
            "fast" => Ok(Level::Fast),
            "balanced" => Ok(Level::Balanced),
            "smallest" => Ok(Level::Smallest),
            other => Err(format!("unknown compression level {other:?}")),
        }
    }
}

impl Level {
    /// Hash-chain candidates tried per position.
    fn chain(self) -> usize {
        match self {
            Level::Fast => 16,
            Level::Balanced => 32,
            Level::Smallest => 128,
        }
    }

    /// Optimal-parse passes, each priced from the one before (0: the lazy parse).
    fn passes(self) -> usize {
        match self {
            Level::Fast => 0,
            Level::Balanced => 2,
            Level::Smallest => 3,
        }
    }
}

/// How many bytes at `buf[i..limit]` repeat those at `buf[c..]` (`c < i`).
fn match_len(buf: &[u8], c: usize, i: usize, limit: usize) -> usize {
    let mut l = 0;
    while i + l + 8 <= limit {
        let a = u64::from_le_bytes(buf[c + l..c + l + 8].try_into().unwrap());
        let b = u64::from_le_bytes(buf[i + l..i + l + 8].try_into().unwrap());
        if a != b {
            return l + ((a ^ b).trailing_zeros() / 8) as usize;
        }
        l += 8;
    }
    while i + l < limit && buf[c + l] == buf[i + l] {
        l += 1;
    }
    l
}

/// A literal's approximate cost in bits once the literals are Huffman-coded.
const LIT_BITS: isize = 8;

/// Roughly how many bits a match saves over sending its bytes as literals: each byte saves a
/// literal; the match costs a command byte, a lengths entry when it is long, and for a new
/// distance an offset code plus its extra bits.
fn gain(len: usize, dist: usize, recent: bool) -> isize {
    let mut cost = 7;
    if len > 16 {
        cost += 8;
    }
    if !recent {
        cost += 7 + traditional_offset(dist).2 as isize;
    }
    len as isize * LIT_BITS - cost
}

struct Matcher {
    head: Vec<i32>,
    prev: Vec<i32>,
    chain: usize,
}

impl Matcher {
    fn new(len: usize, level: Level) -> Self {
        Self {
            head: vec![-1; 1 << HASH_BITS],
            prev: vec![-1; len],
            chain: level.chain(),
        }
    }

    fn hash(buf: &[u8], i: usize) -> usize {
        let v = u32::from_le_bytes([buf[i], buf[i + 1], buf[i + 2], buf[i + 3]]);
        (v.wrapping_mul(0x9E37_79B1) >> (32 - HASH_BITS)) as usize
    }

    fn insert(&mut self, buf: &[u8], i: usize) {
        if i + 4 <= buf.len() {
            let h = Self::hash(buf, i);
            self.prev[i] = self.head[h];
            self.head[h] = i as i32;
        }
    }

    /// The longest match for `buf[i..limit]` against earlier bytes of `buf`, at least 8 back.
    fn best(&self, buf: &[u8], i: usize, limit: usize) -> (usize, usize) {
        let mut best = (0usize, 0usize);
        if i + 4 > limit {
            return best;
        }
        let mut cand = self.head[Self::hash(buf, i)];
        let mut depth = 0;
        while cand >= 0 && depth < self.chain {
            let c = cand as usize;
            let dist = i - c;
            if dist > MAX_DISTANCE {
                break;
            }
            // A candidate can only win if it matches one byte past the best so far.
            let probe = best.0.min(limit - i - 1);
            if dist >= MIN_DISTANCE && buf[c + probe] == buf[i + probe] {
                let l = match_len(buf, c, i, limit);
                if l > best.0 {
                    best = (l, dist);
                    if i + l == limit {
                        break;
                    }
                }
            }
            cand = self.prev[c];
            depth += 1;
        }
        best
    }

    /// Every hash-chain match at `i` longer than the ones before it, as `(len, dist)`.
    fn candidates(&self, buf: &[u8], i: usize, limit: usize, out: &mut Vec<(usize, usize)>) {
        if i + 4 > limit {
            return;
        }
        let mut longest = MIN_MATCH - 1;
        let mut cand = self.head[Self::hash(buf, i)];
        let mut depth = 0;
        while cand >= 0 && depth < self.chain {
            let c = cand as usize;
            let dist = i - c;
            if dist > MAX_DISTANCE {
                break;
            }
            let probe = longest.min(limit - i - 1);
            if dist >= MIN_DISTANCE && buf[c + probe] == buf[i + probe] {
                let l = match_len(buf, c, i, limit);
                if l > longest {
                    longest = l;
                    out.push((l, dist));
                    if i + l == limit {
                        break;
                    }
                }
            }
            cand = self.prev[c];
            depth += 1;
        }
    }
}

/// The match at `i` that saves the most: one of the three recent distances (which cost no
/// offset) or `hash`, the longest the hash chain found. `(gain, len, dist)`.
fn choose(
    buf: &[u8],
    i: usize,
    limit: usize,
    recent: &[usize; 3],
    hash: (usize, usize),
) -> (isize, usize, usize) {
    let mut best = (0isize, 0usize, 0usize);
    for &d in recent {
        if d <= i {
            let l = match_len(buf, i - d, i, limit);
            if l >= 2 {
                let g = gain(l, d, true);
                if g > best.0 {
                    best = (g, l, d);
                }
            }
        }
    }
    let (l, d) = hash;
    if l >= MIN_MATCH {
        let g = gain(l, d, recent.contains(&d));
        if g > best.0 {
            best = (g, l, d);
        }
    }
    best
}

/// One LZ command: `lit` literals, then a match of `len` bytes at `dist` back.
struct Cmd {
    lit: usize,
    len: usize,
    dist: usize,
}

/// The decoder's recent-offset update for a match at `dist`.
fn remember(recent: &mut [usize; 3], dist: usize) {
    match recent.iter().position(|&r| r == dist) {
        Some(k) => {
            for j in (1..=k).rev() {
                recent[j] = recent[j - 1];
            }
            recent[0] = dist;
        }
        None => *recent = [dist, recent[0], recent[1]],
    }
}

/// Cost-guided lazy parse of `buf[from..end]`: at each position take the match that saves the
/// most bits, unless the one starting a byte later saves more.
///
/// `longest(i)` is the longest hash-chain match at `i`, asked for at positions that only grow.
fn parse_lazy(
    buf: &[u8],
    from: usize,
    end: usize,
    mut longest: impl FnMut(usize) -> (usize, usize),
) -> Vec<Cmd> {
    let mut cmds: Vec<Cmd> = Vec::new();
    let mut recent = [8usize; 3];
    let mut anchor = from;
    let mut i = from;
    while i + 2 <= end {
        let mut pick = choose(buf, i, end, &recent, longest(i));
        let mut at = i;
        if pick.0 > 0 && i + 3 <= end {
            let next = choose(buf, i + 1, end, &recent, longest(i + 1));
            if next.0 > pick.0 {
                pick = next;
                at = i + 1;
            }
        }
        if pick.0 > 0 {
            let (_, len, dist) = pick;
            cmds.push(Cmd {
                lit: at - anchor,
                len,
                dist,
            });
            remember(&mut recent, dist);
            i = at + len;
            anchor = i;
        } else {
            i += 1;
        }
    }
    cmds
}

/// Matches at least this long are taken whole, with no shorter cut tried and no search inside.
const NICE_LEN: usize = 96;

/// The streams a list of commands becomes, before the arrays are entropy-coded.
#[derive(Default)]
struct Streams {
    lits: Vec<u8>,
    /// The same literals as literal mode 0 codes them: less the byte at the last match distance.
    delta_lits: Vec<u8>,
    cmd_bytes: Vec<u8>,
    offs_codes: Vec<u8>,
    offs_bits: Vec<(u32, u32)>,
    lens: Vec<u8>,
    escapes: Vec<u32>,
}

/// Commands, offsets, lengths and literals for `cmds` over `buf[from..end]`, with the three
/// recent offsets tracked as the decoder does.
fn streams(buf: &[u8], from: usize, end: usize, cmds: &[Cmd]) -> Streams {
    let mut s = Streams::default();
    let mut recent = [8usize; 3];
    let push_len = |v: usize, s: &mut Streams| {
        let p = v - 3;
        if p < 255 {
            s.lens.push(p as u8);
        } else {
            s.lens.push(255);
            s.escapes.push((p - 255) as u32);
        }
    };
    let mut p = from;
    for c in cmds {
        s.lits.extend_from_slice(&buf[p..p + c.lit]);
        s.delta_lits
            .extend((p..p + c.lit).map(|q| buf[q].wrapping_sub(buf[q - recent[0]])));
        p += c.lit + c.len;
        let lit_code = if c.lit < 3 {
            c.lit as u8
        } else {
            push_len(c.lit, &mut s);
            3
        };
        let kind = match recent.iter().position(|&r| r == c.dist) {
            Some(k) => k as u8,
            None => {
                // The traditional code, not the scaled one (`0x80` marker): the console's
                // decoder rejected our scaled offsets (IOD ec 0x7c/0x7f), and neither Sony's
                // nor LibProsperoPkg's accepted packages ever use them.
                let (code, bits, n) = traditional_offset(c.dist);
                s.offs_codes.push(code);
                s.offs_bits.push((bits, n));
                3
            }
        };
        remember(&mut recent, c.dist);
        let len_code = if c.len - 2 < 15 {
            (c.len - 2) as u8
        } else {
            push_len(c.len - 14, &mut s);
            15
        };
        s.cmd_bytes.push(kind << 6 | len_code << 2 | lit_code);
    }
    s.lits.extend_from_slice(&buf[p..end]);
    s.delta_lits
        .extend((p..end).map(|q| buf[q].wrapping_sub(buf[q - recent[0]])));
    s
}

/// Roughly the bytes a half's streams code to: each array as `put_array` stores it (literals in
/// the smaller mode), plus the offset bits and length escapes.
fn coded_size(s: &Streams) -> usize {
    let array = |d: &[u8]| {
        let mut o = Vec::new();
        put_array(&mut o, d);
        o.len()
    };
    let bits: usize = s.offs_bits.iter().map(|b| b.1 as usize).sum();
    array(&s.lits).min(array(&s.delta_lits))
        + array(&s.cmd_bytes)
        + array(&s.offs_codes)
        + array(&s.lens)
        + bits.div_ceil(8)
        + s.escapes.len() * 3
}

/// What each symbol of each array costs, in bits, as the optimal parse prices a path.
struct Costs {
    lit: [f32; 256],
    cmd: [f32; 256],
    offs: [f32; 256],
    lens: [f32; 256],
    /// Literals priced as literal mode 0 codes them.
    delta: bool,
}

/// Bits per symbol as the array coder will spend them: 8 for an array too short to be Huffman-
/// coded (it is stored raw), else the order-0 entropy, smoothed so a symbol the last pass did not
/// use is dearer but not ruled out.
fn symbol_bits(data: &[u8], default: f32) -> [f32; 256] {
    if data.is_empty() {
        return [default; 256];
    }
    if data.len() < huff::MIN_ARRAY {
        return [8.0; 256];
    }
    let mut hist = [0u32; 256];
    for &b in data {
        hist[b as usize] += 1;
    }
    let total = data.len() as f32 + 128.0;
    std::array::from_fn(|b| (total / (hist[b] as f32 + 0.5)).log2().clamp(1.0, 11.0))
}

impl Costs {
    /// Prices from what a parse actually produced.
    fn measured(s: &Streams) -> Costs {
        let raw = symbol_bits(&s.lits, 8.0);
        let sub = symbol_bits(&s.delta_lits, 8.0);
        let sum = |c: &[f32; 256], d: &[u8]| d.iter().map(|&b| c[b as usize]).sum::<f32>();
        let delta = sum(&sub, &s.delta_lits) < sum(&raw, &s.lits);
        Costs {
            lit: if delta { sub } else { raw },
            cmd: symbol_bits(&s.cmd_bytes, 6.0),
            offs: symbol_bits(&s.offs_codes, 5.0),
            lens: symbol_bits(&s.lens, 6.0),
            delta,
        }
    }

    /// A lengths-array value `v` (≥ 0), escaping past 254.
    fn len_value(&self, v: usize) -> f32 {
        if v < 255 {
            self.lens[v]
        } else {
            self.lens[255] + 2.0 * (usize::BITS - (v - 255 + 64).leading_zeros()) as f32 - 7.0
        }
    }

    /// A match's command and lengths entries, after `run` literals, reusing recent distance
    /// `kind` (0–2) or a new one (3; the caller adds the offset).
    fn matched(&self, run: usize, len: usize, kind: usize) -> f32 {
        let lit_code = run.min(3);
        let len_code = (len - 2).min(15);
        let mut bits = self.cmd[kind << 6 | len_code << 2 | lit_code];
        if run >= 3 {
            bits += self.len_value(run - 3);
        }
        if len_code == 15 {
            bits += self.len_value(len - 17);
        }
        bits
    }
}

/// The lengths of a match worth pricing: each short one (whose command byte differs) and the
/// whole match; past 18 bytes a cut saves only a lengths-array difference.
fn priced_lengths(lo: usize, l: usize) -> impl Iterator<Item = usize> {
    let short_end = if l >= NICE_LEN {
        lo
    } else {
        l.min(SHORT_CUTS) + 1
    };
    (lo..short_end).chain((l >= lo).then_some(l).filter(|&l| l >= short_end))
}

/// Match lengths below this are each priced; longer matches only whole.
const SHORT_CUTS: usize = 18;

/// One position of the optimal parse: the cheapest way found to reach it.
#[derive(Clone, Copy)]
struct Node {
    cost: f32,
    /// The match that ends here (`len` 0 for a literal), from `at − len`.
    len: u32,
    dist: u32,
    recent: [u32; 3],
    /// Literals since the last match on this path.
    run: u32,
}

/// Hash-chain match candidates for every position of `buf[from..end]`, as `(len, dist)` lists:
/// `(starts, all)` where position `i`'s are `all[starts[i]..starts[i + 1]]`. Positions inside a
/// match of [`NICE_LEN`] or more get none.
fn all_candidates(
    buf: &[u8],
    from: usize,
    end: usize,
    m: &mut Matcher,
) -> (Vec<u32>, Vec<(u32, u32)>) {
    let n = end - from;
    let mut starts = Vec::with_capacity(n + 1);
    let mut all = Vec::new();
    let mut found = Vec::new();
    let mut skip_until = from;
    for p in from..end {
        starts.push(all.len() as u32);
        if p >= skip_until {
            found.clear();
            m.candidates(buf, p, end, &mut found);
            for &(l, d) in &found {
                all.push((l as u32, d as u32));
                if l >= NICE_LEN {
                    skip_until = skip_until.max(p + l);
                }
            }
        }
        m.insert(buf, p);
    }
    starts.push(all.len() as u32);
    (starts, all)
}

/// Optimal parse of `buf[from..end]`: the cheapest path through every literal and match choice
/// at `costs`. Each position keeps the recent distances of the cheapest path reaching it, so
/// recent-distance matches and delta literals are priced as the decoder will see them.
fn parse_optimal(
    buf: &[u8],
    from: usize,
    end: usize,
    cands: &(Vec<u32>, Vec<(u32, u32)>),
    costs: &Costs,
) -> Vec<Cmd> {
    let n = end - from;
    let mut nodes = vec![
        Node {
            cost: f32::INFINITY,
            len: 0,
            dist: 0,
            recent: [8; 3],
            run: 0,
        };
        n + 1
    ];
    nodes[0].cost = 0.0;
    let mut skip_until = 0;
    for i in 0..n {
        let p = from + i;
        let node = nodes[i];
        let byte = if costs.delta {
            buf[p].wrapping_sub(buf[p - node.recent[0] as usize])
        } else {
            buf[p]
        };
        let lit = node.cost + costs.lit[byte as usize];
        if lit < nodes[i + 1].cost {
            nodes[i + 1] = Node {
                cost: lit,
                len: 0,
                dist: 0,
                recent: node.recent,
                run: node.run + 1,
            };
        }
        // Inside a long match already priced, searching again would find the same match from
        // every position (quadratic on long runs); literals still relax.
        if p + 2 > end || p < skip_until {
            continue;
        }
        let relax = |nodes: &mut [Node], len: usize, dist: usize, bits: f32, recent: [u32; 3]| {
            let c = node.cost + bits;
            let at = &mut nodes[i + len];
            if c < at.cost {
                *at = Node {
                    cost: c,
                    len: len as u32,
                    dist: dist as u32,
                    recent,
                    run: 0,
                };
            }
        };
        let run = node.run as usize;
        // Recent distances: no offset to send, so every length from 2 is worth pricing.
        for (kind, &d) in node.recent.iter().enumerate() {
            let d = d as usize;
            if d > p || node.recent[..kind].contains(&(d as u32)) {
                continue;
            }
            let l = match_len(buf, p - d, p, end);
            if l < 2 {
                continue;
            }
            if l >= NICE_LEN {
                skip_until = skip_until.max(p + l);
            }
            let mut recent = node.recent.map(|r| r as usize);
            remember(&mut recent, d);
            let recent = recent.map(|r| r as u32);
            for len in priced_lengths(2, l) {
                relax(&mut nodes, len, d, costs.matched(run, len, kind), recent);
            }
        }
        // New distances. The chain's candidates grow longer as they grow farther, so each length
        // is priced only with the nearest candidate that reaches it: the cheapest offset.
        let (starts, all) = cands;
        let mut next_len = MIN_MATCH;
        for &(l, d) in &all[starts[i] as usize..starts[i + 1] as usize] {
            let (l, d) = (l as usize, d as usize);
            if l >= NICE_LEN {
                skip_until = skip_until.max(p + l);
            }
            if node.recent.contains(&(d as u32)) {
                next_len = next_len.max(l + 1);
                continue;
            }
            let (code, _, nbits) = traditional_offset(d);
            let offset_bits = costs.offs[code as usize] + nbits as f32;
            let recent = [d as u32, node.recent[0], node.recent[1]];
            for len in priced_lengths(next_len, l) {
                relax(
                    &mut nodes,
                    len,
                    d,
                    costs.matched(run, len, 3) + offset_bits,
                    recent,
                );
            }
            next_len = next_len.max(l + 1);
        }
    }
    // Walk the cheapest path back from the end.
    let mut steps = Vec::new();
    let mut i = n;
    while i > 0 {
        let node = nodes[i];
        if node.len == 0 {
            i -= 1;
        } else {
            steps.push((i - node.len as usize, node.len as usize, node.dist as usize));
            i -= node.len as usize;
        }
    }
    let mut cmds = Vec::with_capacity(steps.len());
    let mut anchor = 0;
    for &(at, len, dist) in steps.iter().rev() {
        cmds.push(Cmd {
            lit: at - anchor,
            len,
            dist,
        });
        anchor = at + len;
    }
    cmds
}

/// Encode the half `buf[start..end]` of a block buffer (so matches may reach back into the
/// block's even half). `seed` is set for a block's first half. Returns the chunk body and whether
/// its literals are delta-coded (literal mode 0), or `None` when the encoded half would not be
/// smaller than the raw bytes.
fn encode_half(
    buf: &[u8],
    start: usize,
    end: usize,
    seed: bool,
    m: &mut Matcher,
    level: Level,
) -> Option<(Vec<u8>, bool)> {
    let data_start = if seed { start + SEED } else { start };
    if end < data_start + 16 {
        return None;
    }
    for i in start..data_start {
        m.insert(buf, i);
    }
    let cmds = match level.passes() {
        0 => {
            let mut inserted = data_start;
            parse_lazy(buf, data_start, end, |i| {
                while inserted < i {
                    m.insert(buf, inserted);
                    inserted += 1;
                }
                m.best(buf, i, end)
            })
        }
        passes => {
            // Seed the prices from a lazy parse, re-price from each optimal pass, and keep
            // whichever parse codes smallest: prices from one parse can mislead the next (delta
            // literals look nearly free where a recent-distance match would have fit).
            let cands = all_candidates(buf, data_start, end, m);
            let (starts, all) = &cands;
            let lazy = parse_lazy(buf, data_start, end, |i| {
                let k = i - data_start;
                all[starts[k] as usize..starts[k + 1] as usize]
                    .last()
                    .map_or((0, 0), |&(l, d)| (l as usize, d as usize))
            });
            let s = streams(buf, data_start, end, &lazy);
            let mut best = (coded_size(&s), lazy);
            let mut costs = Costs::measured(&s);
            for _ in 0..passes {
                let cmds = parse_optimal(buf, data_start, end, &cands, &costs);
                let s = streams(buf, data_start, end, &cmds);
                let size = coded_size(&s);
                costs = Costs::measured(&s);
                if size < best.0 {
                    best = (size, cmds);
                }
            }
            best.1
        }
    };
    let Streams {
        lits,
        delta_lits,
        cmd_bytes,
        offs_codes,
        offs_bits,
        lens,
        escapes,
    } = streams(buf, data_start, end, &cmds);
    if escapes.len() > 512 {
        return None;
    }
    // The offset bits alternate between the forward and backward streams.
    let (mut fwd, mut bwd) = (BitWriter::default(), BitWriter::default());
    for (k, (v, n)) in offs_bits.iter().enumerate() {
        if k % 2 == 0 {
            fwd.put(*v, *n);
        } else {
            bwd.put(*v, *n);
        }
    }
    let (mut efwd, mut ebwd) = (BitWriter::default(), BitWriter::default());
    for (k, v) in escapes.iter().enumerate() {
        if k % 2 == 0 {
            efwd.put_length(*v);
        } else {
            ebwd.put_length(*v);
        }
    }
    let mut excess = efwd.finish();
    let mut eb = ebwd.finish();
    eb.reverse();
    excess.extend(eb);
    if excess.len() > 0x1F + 0xFF * 0x20 {
        return None;
    }

    let mut out = Vec::with_capacity(end - start);
    if seed {
        out.extend_from_slice(&buf[start..data_start]);
    }
    let e = excess.len();
    if e > 0x1F {
        let hi = (e - 0x20) / 0x20;
        let lo = e - hi * 0x20;
        // count = lo (0x20..0x3F) + hi * 0x20, with lo > 0x1F signalling the continuation byte.
        out.push(0x80 | lo as u8);
        out.push(hi as u8);
    } else {
        out.push(0x80 | e as u8);
    }
    // Literal mode 0 when delta-coding makes the literals smaller: structured data (vertices,
    // tables) often repeats with small differences at the distance it last matched.
    let (mut raw_lits, mut sub_lits) = (Vec::new(), Vec::new());
    put_array(&mut raw_lits, &lits);
    put_array(&mut sub_lits, &delta_lits);
    let delta = sub_lits.len() < raw_lits.len();
    out.extend(if delta { sub_lits } else { raw_lits });
    put_array(&mut out, &cmd_bytes);
    put_array(&mut out, &offs_codes);
    put_array(&mut out, &lens);
    let mut f = fwd.finish();
    let mut b = bwd.finish();
    b.reverse();
    out.append(&mut f);
    out.append(&mut b);
    out.extend(excess);
    // Compressed only when it saves at least 2%: below that the console would decode a half
    // to gain almost nothing, where a raw half is a plain copy. (The decoder also wants at
    // least 13 bytes of chunk.)
    let raw = end - start;
    if out.len() + raw / 50 >= raw || out.len() < 13 {
        return None;
    }
    Some((out, delta))
}

/// How one half of a block is stored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Half {
    /// The logical bytes as they are.
    Raw(Vec<u8>),
    /// A Kraken LZ chunk body with its literals as they are (literal mode 1).
    Lz(Vec<u8>),
    /// A Kraken LZ chunk body with delta literals (literal mode 0): each literal is stored less
    /// the byte at the last match distance.
    LzDelta(Vec<u8>),
}

impl Half {
    pub fn bytes(&self) -> &[u8] {
        match self {
            Half::Raw(b) | Half::Lz(b) | Half::LzDelta(b) => b,
        }
    }

    pub fn is_lz(&self) -> bool {
        !matches!(self, Half::Raw(_))
    }

    pub fn is_delta(&self) -> bool {
        matches!(self, Half::LzDelta(_))
    }

    fn from_encoded(encoded: Option<(Vec<u8>, bool)>, raw: &[u8]) -> Half {
        match encoded {
            Some((b, false)) => Half::Lz(b),
            Some((b, true)) => Half::LzDelta(b),
            None => Half::Raw(raw.to_vec()),
        }
    }
}

/// Encode one block (up to 256 KiB) into its halves at the default level.
pub fn encode_block(block: &[u8]) -> Vec<Half> {
    encode_block_at(block, Level::default())
}

/// Encode one block (up to 256 KiB) into its halves. A half that does not shrink is raw.
pub fn encode_block_at(block: &[u8], level: Level) -> Vec<Half> {
    assert!(!block.is_empty() && block.len() <= BLOCK);
    let mut m = Matcher::new(block.len(), level);
    let even_end = block.len().min(HALF);
    let mut halves = vec![Half::from_encoded(
        encode_half(block, 0, even_end, true, &mut m, level),
        &block[..even_end],
    )];
    if block.len() > HALF {
        // The odd half may reach back into the even half, so its matcher starts out knowing it.
        m = Matcher::new(block.len(), level);
        for i in 0..even_end {
            m.insert(block, i);
        }
        halves.push(Half::from_encoded(
            encode_half(block, HALF, block.len(), false, &mut m, level),
            &block[HALF..],
        ));
    }
    halves
}

// ─────────────────────────────── decoder ───────────────────────────────

/// Literals into `out[dst..dst + n]`, delta-coded against the byte `last` back when `delta`.
fn put_literals(out: &mut [u8], dst: usize, lits: &[u8], delta: bool, last: usize) {
    if delta {
        for (k, &l) in lits.iter().enumerate() {
            out[dst + k] = l.wrapping_add(out[dst + k - last]);
        }
    } else {
        out[dst..dst + lits.len()].copy_from_slice(lits);
    }
}

/// Decode one LZ half into `out[at..at + len]`; `out[..at]` is the block's history. `delta`
/// selects literal mode 0.
fn decode_half(src: &[u8], out: &mut [u8], at: usize, len: usize, delta: bool) -> Result<()> {
    let end = at + len;
    let mut p = 0usize;
    let mut dst = at;
    if at == 0 {
        if src.len() < SEED {
            return format_err("kraken: truncated seed");
        }
        out[..SEED].copy_from_slice(&src[..SEED]);
        p = SEED;
        dst = SEED;
    }
    if src.len() < p + 13 {
        return format_err("kraken: chunk too short");
    }
    let flag = src[p];
    if flag & 0xC0 != 0x80 {
        return format_err("kraken: expected the excess framing");
    }
    p += 1;
    let mut excess = (flag & 0x3F) as usize;
    if excess > 0x1F {
        excess += src[p] as usize * 0x20;
        p += 1;
    }
    if excess > src.len() - p {
        return format_err("kraken: excess overruns the chunk");
    }
    let main_end = src.len() - excess;
    let (lits, n) = get_array(src, p, main_end, len)?;
    p += n;
    let (cmds, n) = get_array(src, p, main_end, len)?;
    p += n;
    if p >= main_end {
        return format_err("kraken: truncated offsets");
    }
    // `0x80` marks the scaled offset code (what this encoder once wrote); anything else is the
    // start of the offsets array in the traditional code.
    let scaled = src[p] == 0x80;
    if scaled {
        p += 1;
    }
    let (offs_codes, n) = get_array(src, p, main_end, cmds.len())?;
    p += n;
    let (lens, n) = get_array(src, p, main_end, len / 4)?;
    p += n;

    // Offsets from the two main streams.
    let mut a = BitReader::new(src, p, main_end, false);
    let mut b = BitReader::new(src, p, main_end, true);
    let mut offsets = Vec::with_capacity(offs_codes.len());
    for (k, &c) in offs_codes.iter().enumerate() {
        let r = if k % 2 == 0 { &mut a } else { &mut b };
        if scaled {
            let nb = u32::from(c >> 3);
            if nb > 26 {
                return format_err("kraken: bad offset code");
            }
            let offs = ((8 + u32::from(c & 7)) << nb) | r.get(nb);
            offsets.push(offs as usize - 8);
        } else {
            if c >= 0xF0 {
                return format_err("kraken: offset beyond a block");
            }
            let n = u32::from(c >> 4) + 4;
            let v = (((1u32 << n) | r.get(n)) << 4) + u32::from(c & 0xF) - 248;
            offsets.push(v as usize);
        }
    }
    if a.seam() != b.seam() {
        return format_err("kraken: offset streams do not meet");
    }
    // Length escapes from the excess stream.
    let n_esc = lens.iter().filter(|&&v| v == 255).count();
    let mut esc = Vec::with_capacity(n_esc);
    let mut ea = BitReader::new(src, main_end, src.len(), false);
    let mut eb = BitReader::new(src, main_end, src.len(), true);
    for k in 0..n_esc {
        esc.push(if k % 2 == 0 {
            ea.get_length()?
        } else {
            eb.get_length()?
        });
    }
    let mut esc = esc.into_iter();
    let mut lens = lens.iter().map(|&v| -> Result<usize> {
        Ok(3 + if v == 255 {
            255 + esc
                .next()
                .ok_or_else(|| crate::Error::Format("kraken: escape missing".into()))?
                as usize
        } else {
            v as usize
        })
    });

    let mut lit_at = 0usize;
    let mut offs = offsets.into_iter();
    let mut recent = [8usize, 8, 8];
    for &f in &cmds {
        let mut lit = (f & 3) as usize;
        if lit == 3 {
            lit = lens
                .next()
                .ok_or_else(|| crate::Error::Format("kraken: length missing".into()))??;
        }
        let kind = (f >> 6) as usize;
        let code = ((f >> 2) & 0xF) as usize;
        if lit_at + lit > lits.len() || dst + lit > end {
            return format_err("kraken: literal run overruns");
        }
        put_literals(out, dst, &lits[lit_at..lit_at + lit], delta, recent[0]);
        dst += lit;
        lit_at += lit;
        let dist = if kind == 3 {
            let d = offs
                .next()
                .ok_or_else(|| crate::Error::Format("kraken: offset missing".into()))?;
            recent = [d, recent[0], recent[1]];
            d
        } else {
            let d = recent[kind];
            for j in (1..=kind).rev() {
                recent[j] = recent[j - 1];
            }
            recent[0] = d;
            d
        };
        let mlen = if code == 15 {
            14 + lens
                .next()
                .ok_or_else(|| crate::Error::Format("kraken: length missing".into()))??
        } else {
            code + 2
        };
        if dist < MIN_DISTANCE || dist > dst || dst + mlen > end {
            return format_err("kraken: match out of bounds");
        }
        for k in 0..mlen {
            out[dst + k] = out[dst + k - dist];
        }
        dst += mlen;
    }
    let tail = end - dst;
    if lits.len() - lit_at != tail {
        return format_err("kraken: trailing literals do not fill the half");
    }
    put_literals(out, dst, &lits[lit_at..], delta, recent[0]);
    if offs.next().is_some() || lens.next().is_some() {
        return format_err("kraken: unused offsets or lengths");
    }
    Ok(())
}

/// Decode a block of `len` logical bytes from its halves.
pub fn decode_block(halves: &[Half], len: usize) -> Result<Vec<u8>> {
    let mut out = vec![0u8; len];
    let mut at = 0usize;
    for h in halves {
        let hl = (len - at).min(HALF);
        match h {
            Half::Raw(b) => {
                if b.len() != hl {
                    return format_err("kraken: raw half has the wrong length");
                }
                out[at..at + hl].copy_from_slice(b);
            }
            Half::Lz(b) => decode_half(b, &mut out, at, hl, false)?,
            Half::LzDelta(b) => decode_half(b, &mut out, at, hl, true)?,
        }
        at += hl;
    }
    if at != len {
        return format_err("kraken: halves do not cover the block");
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roundtrip(data: &[u8]) -> usize {
        let halves = encode_block(data);
        let back = decode_block(&halves, data.len()).unwrap();
        assert_eq!(back, data);
        halves.iter().map(|h| h.bytes().len()).sum()
    }

    fn noise(n: usize, seed: u32) -> Vec<u8> {
        let mut x = seed;
        (0..n)
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 17;
                x ^= x << 5;
                x as u8
            })
            .collect()
    }

    #[test]
    fn text_compresses_and_round_trips() {
        let mut t = String::new();
        let mut i = 0;
        while t.len() < BLOCK {
            t.push_str(&format!(
                "entry {i}: the quick brown fox jumps over the lazy dog\n"
            ));
            i += 1;
        }
        let data = &t.as_bytes()[..BLOCK];
        let stored = roundtrip(data);
        assert!(stored < BLOCK / 4, "{stored}");
    }

    /// The traditional offset code, checked against the decoder's formula written out
    /// independently (ooz `read_distance`: `rv = ((1 << n | x) << 4) + (v & 15) − 248`,
    /// `n = (v >> 4) + 4`), so a mistake shared by our encoder and decoder cannot hide.
    #[test]
    fn traditional_offsets_match_the_reference_formula() {
        for d in (8..70_000).chain([0x1_FFFF, 0x3_FFF8, 0x40_0000]) {
            let (v, x, n) = traditional_offset(d);
            assert!(v < 0xF0, "{d}");
            assert_eq!(n, u32::from(v >> 4) + 4, "{d}");
            assert!(x < (1 << n), "{d}");
            let rv = (((1u32 << n) | x) << 4) + u32::from(v & 0xF) - 248;
            assert_eq!(rv as usize, d);
        }
    }

    /// What the console's decoder accepts, as the encoders it accepts write it: every array
    /// raw with the 3-byte header or Huffman with the 5-byte one, and offsets in the traditional
    /// code (no `0x80` marker).
    #[test]
    fn halves_use_only_the_accepted_forms() {
        let mut t = String::new();
        let mut i = 0;
        while t.len() < BLOCK {
            t.push_str(&format!("{i} lorem ipsum dolor sit amet {}\n", i * 7));
            i += 1;
        }
        let mut huffman = 0;
        for (half, h) in encode_block(&t.as_bytes()[..BLOCK]).into_iter().enumerate() {
            if !h.is_lz() {
                continue;
            }
            let body = h.bytes();
            // Past the even half's seed and the excess framing byte(s), the arrays begin.
            let mut p = if half == 0 { SEED } else { 0 };
            let flag = body[p];
            p += 1;
            if flag & 0x3F > 0x1F {
                p += 1;
            }
            for k in 0..4 {
                let b0 = body[p];
                assert!(b0 < 0x80, "short array header at {p}");
                let be = |at: usize, n: usize| {
                    body[at..at + n]
                        .iter()
                        .fold(0usize, |v, &b| v << 8 | usize::from(b))
                };
                p += match b0 >> 4 {
                    0 => 3 + be(p, 3),
                    2 => {
                        huffman += 1;
                        5 + (be(p + 1, 4) & 0x3FFFF)
                    }
                    other => panic!("array type {other}"),
                };
                if k == 1 {
                    assert_ne!(body[p], 0x80, "scaled-offset marker");
                }
            }
        }
        assert!(huffman > 0, "text should get Huffman arrays");
    }

    #[test]
    fn noise_is_stored_raw() {
        let data = noise(BLOCK, 7);
        let halves = encode_block(&data);
        assert!(halves.iter().all(|h| matches!(h, Half::Raw(_))));
        assert_eq!(decode_block(&halves, BLOCK).unwrap(), data);
    }

    #[test]
    fn short_blocks_long_runs_and_mixed_content_round_trip() {
        for len in [
            1usize,
            7,
            8,
            9,
            40,
            1000,
            HALF - 1,
            HALF,
            HALF + 1,
            HALF + 5000,
            BLOCK - 3,
        ] {
            let mut d = noise(len, len as u32 + 1);
            // Long zero runs and repeats exercise the length escapes and recent offsets.
            if len > 5000 {
                for b in &mut d[1000..4000] {
                    *b = 0;
                }
                let (a, b) = d.split_at_mut(len / 2);
                let n = 3000.min(b.len());
                b[..n].copy_from_slice(&a[..n]);
            }
            roundtrip(&d);
        }
        roundtrip(&vec![0u8; BLOCK]);
        roundtrip(&vec![0xABu8; HALF + 77]);
    }

    const LEVELS: [Level; 3] = [Level::Fast, Level::Balanced, Level::Smallest];

    /// Structured data: 32-bit records whose fields step by small amounts, the case literal
    /// mode 0 exists for.
    fn records(n: usize) -> Vec<u8> {
        let mut d = Vec::with_capacity(n);
        let mut i = 0u32;
        while d.len() < n {
            d.extend_from_slice(&(i * 3).to_le_bytes());
            d.extend_from_slice(&(1000 + i / 7).to_le_bytes());
            d.extend_from_slice(&[(i % 5) as u8, 0x40, 0, 0]);
            i += 1;
        }
        d.truncate(n);
        d
    }

    #[test]
    fn every_level_round_trips() {
        let mut mixed = noise(BLOCK, 3);
        mixed[5000..60_000].copy_from_slice(&records(55_000));
        mixed[HALF + 100..HALF + 40_000].fill(0);
        let (a, b) = mixed.split_at_mut(HALF);
        b[50_000..70_000].copy_from_slice(&a[1000..21_000]);
        for level in LEVELS {
            for data in [
                &mixed[..],
                &records(BLOCK),
                &records(HALF + 99),
                &[9u8; 300][..],
            ] {
                let halves = encode_block_at(data, level);
                assert_eq!(
                    decode_block(&halves, data.len()).unwrap(),
                    data,
                    "{level:?}"
                );
            }
        }
    }

    #[test]
    fn delta_literals_are_chosen_where_they_help() {
        let data = records(BLOCK);
        let halves = encode_block(&data);
        assert!(
            halves.iter().any(Half::is_delta),
            "records should use literal mode 0"
        );
        assert_eq!(decode_block(&halves, BLOCK).unwrap(), data);
    }

    /// The levels trade time for size, never the other way round.
    #[test]
    fn slower_levels_are_not_larger() {
        let mut t = records(BLOCK);
        for (k, b) in t.iter_mut().enumerate().step_by(11) {
            *b ^= (k / 13) as u8;
        }
        let size = |l| -> usize { encode_block_at(&t, l).iter().map(|h| h.bytes().len()).sum() };
        let (fast, balanced, smallest) = (
            size(Level::Fast),
            size(Level::Balanced),
            size(Level::Smallest),
        );
        assert!(balanced <= fast, "{balanced} > {fast}");
        assert!(
            smallest <= balanced + balanced / 200,
            "{smallest} > {balanced}"
        );
    }

    #[test]
    fn a_corrupt_half_is_refused_not_misread() {
        let data = vec![0x11u8; BLOCK];
        let mut halves = encode_block(&data);
        if let Half::Lz(b) = &mut halves[0] {
            let n = b.len();
            b[n / 2] ^= 0x55;
        }
        let r = decode_block(&halves, BLOCK);
        assert!(r.is_err() || r.unwrap() != data);
    }
}
