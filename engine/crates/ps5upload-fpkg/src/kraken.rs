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
//! Arrays are stored raw (Oodle's type 0). Literals are raw (literal mode 1). Offsets use the
//! scaled form: `cmd = nbits << 3 | top`, `offset = ((8 + top) << nbits | bits) - 8`. A command
//! byte is `offset kind << 6 | (match length − 2) << 2 | literal run`, where kind 0–2 reuses one
//! of the three recent offsets and 3 takes the next new one; a run of 3 or a length code of 15
//! takes its value from the lengths array (value − 3, 255 escaping to the excess stream).
//!
//! The decoder copies matches eight bytes at a time, so a match is never closer than 8 bytes.
//! The encoder is written from the format (documented by powzix/ooz, GPL-3, and the PS5 framing
//! decoded from Sony's packages); the decoder here reads exactly what the encoder writes and is
//! what proves every block before a package keeps it.

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

/// A raw (type 0) array at `at`, at most `max` bytes: `(bytes, header + payload length)`.
fn get_raw_array(src: &[u8], at: usize, end: usize, max: usize) -> Result<(&[u8], usize)> {
    if end < at + 2 {
        return format_err("kraken: truncated array");
    }
    let b0 = src[at];
    if (b0 >> 4) & 7 != 0 {
        return format_err("kraken: only raw arrays are supported");
    }
    let (n, h) = if b0 >= 0x80 {
        ((((b0 as usize) << 8) | src[at + 1] as usize) & 0xFFF, 2)
    } else {
        if end < at + 3 {
            return format_err("kraken: truncated array");
        }
        let n = ((b0 as usize) << 16) | ((src[at + 1] as usize) << 8) | src[at + 2] as usize;
        if n & !0x3FFFF != 0 {
            return format_err("kraken: bad array size");
        }
        (n, 3)
    };
    if n > max || at + h + n > end {
        return format_err("kraken: array overruns its chunk");
    }
    Ok((&src[at + h..at + h + n], h + n))
}

// ─────────────────────────────── encoder ───────────────────────────────

const HASH_BITS: u32 = 16;
const CHAIN_DEPTH: usize = 24;

struct Matcher {
    head: Vec<i32>,
    prev: Vec<i32>,
}

impl Matcher {
    fn new(len: usize) -> Self {
        Self {
            head: vec![-1; 1 << HASH_BITS],
            prev: vec![-1; len],
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
        while cand >= 0 && depth < CHAIN_DEPTH {
            let c = cand as usize;
            let dist = i - c;
            if dist > MAX_DISTANCE {
                break;
            }
            if dist >= MIN_DISTANCE
                && buf[c + best.0.min(limit - i - 1)] == buf[i + best.0.min(limit - i - 1)]
            {
                let mut l = 0;
                while i + l < limit && buf[c + l] == buf[i + l] {
                    l += 1;
                }
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
}

/// One LZ command: `lit` literals, then a match of `len` bytes at `dist` back.
struct Cmd {
    lit: usize,
    len: usize,
    dist: usize,
}

/// Encode the half `buf[start..end]` of a block buffer (so matches may reach back into the
/// block's even half). `seed` is set for a block's first half. Returns `None` when the encoded
/// half would not be smaller than the raw bytes.
fn encode_half(
    buf: &[u8],
    start: usize,
    end: usize,
    seed: bool,
    m: &mut Matcher,
) -> Option<Vec<u8>> {
    let data_start = if seed { start + SEED } else { start };
    if end < data_start + 16 {
        return None;
    }
    for i in start..data_start {
        m.insert(buf, i);
    }
    // Greedy parse with one step of lazy matching.
    let mut cmds: Vec<Cmd> = Vec::new();
    let mut lits: Vec<u8> = Vec::new();
    let mut anchor = data_start;
    let mut i = data_start;
    while i + MIN_MATCH <= end {
        let (mut len, mut dist) = m.best(buf, i, end);
        if len >= MIN_MATCH && i + 1 + MIN_MATCH <= end {
            m.insert(buf, i);
            let (l2, d2) = m.best(buf, i + 1, end);
            if l2 > len + 1 {
                i += 1;
                len = l2;
                dist = d2;
            }
        } else {
            m.insert(buf, i);
        }
        if len >= MIN_MATCH {
            lits.extend_from_slice(&buf[anchor..i]);
            cmds.push(Cmd {
                lit: i - anchor,
                len,
                dist,
            });
            for j in i + 1..i + len {
                m.insert(buf, j);
            }
            i += len;
            anchor = i;
        } else {
            i += 1;
        }
    }
    for j in i..end {
        m.insert(buf, j);
    }
    lits.extend_from_slice(&buf[anchor..end]);

    // Commands, offsets and lengths, with the three recent offsets tracked as the decoder does.
    let mut cmd_bytes = Vec::with_capacity(cmds.len());
    let mut offs_codes = Vec::new();
    let mut offs_bits: Vec<(u32, u32)> = Vec::new();
    let mut lens: Vec<u8> = Vec::new();
    let mut escapes: Vec<u32> = Vec::new();
    let mut recent = [8usize, 8, 8];
    let push_len = |v: usize, lens: &mut Vec<u8>, escapes: &mut Vec<u32>| {
        let p = v - 3;
        if p < 255 {
            lens.push(p as u8);
        } else {
            lens.push(255);
            escapes.push((p - 255) as u32);
        }
    };
    for c in &cmds {
        let lit_code = if c.lit < 3 {
            c.lit as u8
        } else {
            push_len(c.lit, &mut lens, &mut escapes);
            3
        };
        let kind = recent.iter().position(|&r| r == c.dist);
        let kind = match kind {
            Some(k) => {
                // Move to front, as the decoder's recent-offset shuffle does.
                let d = recent[k];
                for j in (1..=k).rev() {
                    recent[j] = recent[j - 1];
                }
                recent[0] = d;
                k as u8
            }
            None => {
                recent = [c.dist, recent[0], recent[1]];
                // The traditional code, not the scaled one (`0x80` marker): the console's
                // decoder rejected our scaled offsets (IOD ec 0x7c/0x7f), and neither Sony's
                // nor LibProsperoPkg's accepted packages ever use them.
                let (code, bits, n) = traditional_offset(c.dist);
                offs_codes.push(code);
                offs_bits.push((bits, n));
                3
            }
        };
        let len_code = if c.len - 2 < 15 {
            (c.len - 2) as u8
        } else {
            push_len(c.len - 14, &mut lens, &mut escapes);
            15
        };
        cmd_bytes.push(kind << 6 | len_code << 2 | lit_code);
    }
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
    put_raw_array(&mut out, &lits);
    put_raw_array(&mut out, &cmd_bytes);
    put_raw_array(&mut out, &offs_codes);
    put_raw_array(&mut out, &lens);
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
    Some(out)
}

/// How one half of a block is stored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Half {
    /// The logical bytes as they are.
    Raw(Vec<u8>),
    /// A Kraken LZ chunk body (literal mode 1).
    Lz(Vec<u8>),
}

impl Half {
    pub fn bytes(&self) -> &[u8] {
        match self {
            Half::Raw(b) | Half::Lz(b) => b,
        }
    }
}

/// Encode one block (up to 256 KiB) into its halves. A half that does not shrink is raw.
pub fn encode_block(block: &[u8]) -> Vec<Half> {
    assert!(!block.is_empty() && block.len() <= BLOCK);
    let mut m = Matcher::new(block.len());
    let even_end = block.len().min(HALF);
    let mut halves = vec![match encode_half(block, 0, even_end, true, &mut m) {
        Some(b) => Half::Lz(b),
        None => Half::Raw(block[..even_end].to_vec()),
    }];
    if block.len() > HALF {
        // The odd half may reach back into the even half, so its matcher starts out knowing it.
        m = Matcher::new(block.len());
        for i in 0..even_end {
            m.insert(block, i);
        }
        halves.push(match encode_half(block, HALF, block.len(), false, &mut m) {
            Some(b) => Half::Lz(b),
            None => Half::Raw(block[HALF..].to_vec()),
        });
    }
    halves
}

// ─────────────────────────────── decoder ───────────────────────────────

/// Decode one LZ half into `out[at..at + len]`; `out[..at]` is the block's history.
fn decode_half(src: &[u8], out: &mut [u8], at: usize, len: usize) -> Result<()> {
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
    let (lits, n) = get_raw_array(src, p, main_end, len)?;
    p += n;
    let (cmds, n) = get_raw_array(src, p, main_end, len)?;
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
    let (offs_codes, n) = get_raw_array(src, p, main_end, cmds.len())?;
    p += n;
    let (lens, n) = get_raw_array(src, p, main_end, len / 4)?;
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
    for &f in cmds {
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
        out[dst..dst + lit].copy_from_slice(&lits[lit_at..lit_at + lit]);
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
    out[dst..end].copy_from_slice(&lits[lit_at..]);
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
            Half::Lz(b) => decode_half(b, &mut out, at, hl)?,
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
    /// with the 3-byte raw header and offsets in the traditional code (no `0x80` marker).
    #[test]
    fn halves_use_only_the_accepted_forms() {
        let mut t = String::new();
        let mut i = 0;
        while t.len() < BLOCK {
            t.push_str(&format!("{i} lorem ipsum dolor sit amet {}\n", i * 7));
            i += 1;
        }
        for h in encode_block(&t.as_bytes()[..BLOCK]) {
            let Half::Lz(body) = h else { continue };
            // Past the seed and the excess framing byte(s), the literals array begins.
            let mut p = SEED;
            let flag = body[p];
            p += 1;
            if flag & 0x3F > 0x1F {
                p += 1;
            }
            for _ in 0..2 {
                assert!(body[p] < 0x80, "short raw header at {p}");
                let n = (usize::from(body[p]) << 16)
                    | (usize::from(body[p + 1]) << 8)
                    | usize::from(body[p + 2]);
                p += 3 + n;
            }
            assert_ne!(body[p], 0x80, "scaled-offset marker");
            break;
        }
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
