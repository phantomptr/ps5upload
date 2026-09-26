//! Kraken's Huffman-coded arrays (array type 2), in the one form the console's hardware decoder
//! is known to accept: every Huffman array in LibProsperoPkg's accepted packages is type 2 with
//! the 5-byte header and the "old, full" code-length table, so that is all the encoder writes.
//!
//! ```text
//! table (MSB-first)   0 (old form), 1 (full), forced_bits:2, first-symbol-used:1, then
//!                     alternating gamma-coded runs of unused / used symbols; each used symbol's
//!                     code length is a zigzag delta from a running average (avg_x4), written as
//!                     `lz` zeros, a 1, and `forced_bits` low bits of the delta code
//! split (LE16)        the byte length of stream A
//! A | C | B reversed  three LSB-first streams; symbol i goes to A, B, C for i % 3 = 0, 1, 2
//! ```
//!
//! Codes are canonical: lengths 1..=11, shortest first, symbols of one length in table order,
//! and the code must be complete (it fills the decoder's 2048-entry table exactly). The streams
//! must meet exactly: A ends at the split, and C and B, rounded up to whole bytes, fill the rest.

use super::{BitReader, BitWriter};
use crate::{format_err, Result};

const MAX_LEN: u32 = 11;
/// The smallest array worth coding. Measured on a FW 5.10 Phat: Huffman on arrays of every size
/// froze the console at its first hardware-decoded read, while the same package with only arrays
/// of 256 bytes and up launched and played. PSVIETHOA's accepted packages almost never code an
/// array under 48 bytes; below 256 the saving is a few hundred KiB in 800 MiB anyway.
pub(super) const MIN_ARRAY: usize = 256;
const LUT: usize = 1 << MAX_LEN;

/// An LSB-first bit writer.
#[derive(Default)]
struct LsbWriter {
    bytes: Vec<u8>,
    acc: u64,
    n: u32,
}

impl LsbWriter {
    fn put(&mut self, v: u32, bits: u32) {
        self.acc |= u64::from(v) << self.n;
        self.n += bits;
        while self.n >= 8 {
            self.bytes.push(self.acc as u8);
            self.acc >>= 8;
            self.n -= 8;
        }
    }

    fn finish(mut self) -> Vec<u8> {
        if self.n > 0 {
            self.bytes.push(self.acc as u8);
        }
        self.bytes
    }
}

/// Huffman code lengths for `freq`, deepened no further than 11 bits by flattening the
/// frequencies until the tree fits. Every symbol with a frequency gets a length.
fn code_lengths(freq: &[u32; 256]) -> [u8; 256] {
    let mut f = *freq;
    loop {
        let lens = huffman_lengths(&f);
        if lens.iter().all(|&l| u32::from(l) <= MAX_LEN) {
            return lens;
        }
        for x in f.iter_mut().filter(|x| **x > 0) {
            *x = x.div_ceil(2);
        }
    }
}

fn huffman_lengths(f: &[u32; 256]) -> [u8; 256] {
    use std::{cmp::Reverse, collections::BinaryHeap};
    let mut parent: Vec<usize> = Vec::with_capacity(512);
    let mut leaf = [usize::MAX; 256];
    let mut heap = BinaryHeap::new();
    for (s, &w) in f.iter().enumerate().filter(|(_, w)| **w > 0) {
        leaf[s] = parent.len();
        heap.push(Reverse((u64::from(w), parent.len())));
        parent.push(usize::MAX);
    }
    while heap.len() > 1 {
        let Reverse((wa, a)) = heap.pop().unwrap();
        let Reverse((wb, b)) = heap.pop().unwrap();
        let n = parent.len();
        parent.push(usize::MAX);
        parent[a] = n;
        parent[b] = n;
        heap.push(Reverse((wa + wb, n)));
    }
    let mut lens = [0u8; 256];
    for (s, &node) in leaf.iter().enumerate().filter(|(_, n)| **n != usize::MAX) {
        let (mut x, mut d) = (node, 0u8);
        while parent[x] != usize::MAX {
            x = parent[x];
            d += 1;
        }
        lens[s] = d;
    }
    lens
}

/// Canonical codes as the decoder assigns them, bit-reversed for the LSB-first streams:
/// `(code, length)` per symbol.
fn canonical(lens: &[u8; 256]) -> [(u32, u32); 256] {
    let mut codes = [(0u32, 0u32); 256];
    let mut slot = 0u32;
    for l in 1..=MAX_LEN {
        for s in (0..256).filter(|&s| u32::from(lens[s]) == l) {
            let code = slot >> (MAX_LEN - l);
            codes[s] = (code.reverse_bits() >> (32 - l), l);
            slot += 1 << (MAX_LEN - l);
        }
    }
    debug_assert_eq!(slot as usize, LUT, "incomplete code");
    codes
}

/// Elias gamma as the table uses it, for a run of `r` ≥ 1.
fn put_gamma(w: &mut BitWriter, r: u32) {
    let v = r + 1;
    let b = 32 - v.leading_zeros();
    w.put(0, b - 2);
    w.put(v, b);
}

/// The old, full code-length table with `forced_bits = fb`, or `None` when some length's delta
/// needs more leading zeros than the decoder allows for that `fb`.
fn table(lens: &[u8; 256], fb: u32) -> Option<Vec<u8>> {
    let mut w = BitWriter::default();
    w.put(0, 1);
    w.put(1, 1);
    w.put(fb, 2);
    let first = lens[0] != 0;
    w.put(u32::from(first), 1);
    let limit = 20 >> fb;
    let (mut sym, mut avg) = (0usize, 32i32);
    loop {
        if !(sym == 0 && first) {
            let z = lens[sym..].iter().take_while(|&&l| l == 0).count();
            put_gamma(&mut w, z as u32);
            sym += z;
            if sym >= 256 {
                break;
            }
        }
        let n = lens[sym..].iter().take_while(|&&l| l != 0).count();
        put_gamma(&mut w, n as u32);
        for &l in &lens[sym..sym + n] {
            let delta = i32::from(l) - ((avg + 2) >> 2);
            let v = if delta >= 0 {
                2 * delta
            } else {
                -2 * delta - 1
            } as u32;
            let lz = v >> fb;
            if lz > limit {
                return None;
            }
            w.put(0, lz);
            w.put(1, 1);
            w.put(v & ((1 << fb) - 1), fb);
            avg = i32::from(l) + ((3 * avg + 2) >> 2);
        }
        sym += n;
        if sym == 256 {
            break;
        }
    }
    Some(w.finish())
}

/// A type-2 array body (everything after the 5-byte header), or `None` when the data is under
/// [`MIN_ARRAY`] or too large. The caller keeps it only if it beats the raw array.
pub(super) fn encode(data: &[u8]) -> Option<Vec<u8>> {
    if data.len() < MIN_ARRAY || data.len() > 0x4_0000 {
        return None;
    }
    let mut freq = [0u32; 256];
    for &b in data {
        freq[b as usize] += 1;
    }
    if freq.iter().filter(|&&f| f > 0).count() < 2 {
        // The full table needs two symbols; one symbol gets a never-used partner.
        freq[usize::from(data[0] == 0)] = 1;
    }
    let lens = code_lengths(&freq);
    let table = (0..4)
        .filter_map(|fb| table(&lens, fb))
        .min_by_key(Vec::len)?;
    let codes = canonical(&lens);
    let mut s: [LsbWriter; 3] = Default::default();
    for (i, &b) in data.iter().enumerate() {
        let (c, l) = codes[b as usize];
        s[i % 3].put(c, l);
    }
    let [a, b, c] = s.map(LsbWriter::finish);
    if a.len() > 0xFFFF {
        return None;
    }
    let mut out = table;
    out.extend_from_slice(&(a.len() as u16).to_le_bytes());
    out.extend(a);
    out.extend(c);
    out.extend(b.into_iter().rev());
    Some(out)
}

fn get_gamma(r: &mut BitReader) -> Result<usize> {
    let lz = r.bits.leading_zeros();
    if lz > 7 {
        return format_err("kraken: bad Huffman table run");
    }
    Ok(r.get(2 * (lz + 1)) as usize - 1)
}

/// An LSB-first reader over `src[start..end)`, forward or (for stream B) backward; reads past
/// its range see zeros. Counts the bits it consumes.
struct LsbReader<'a> {
    src: &'a [u8],
    next: usize,
    left: usize,
    back: bool,
    acc: u64,
    n: u32,
    used: usize,
}

impl<'a> LsbReader<'a> {
    fn new(src: &'a [u8], start: usize, end: usize, back: bool) -> Self {
        Self {
            src,
            next: if back { end } else { start },
            left: end - start,
            back,
            acc: 0,
            n: 0,
            used: 0,
        }
    }

    fn peek(&mut self) -> usize {
        while self.n < MAX_LEN {
            let byte = if self.left == 0 {
                0
            } else {
                self.left -= 1;
                if self.back {
                    self.next -= 1;
                    self.src[self.next]
                } else {
                    self.next += 1;
                    self.src[self.next - 1]
                }
            };
            self.acc |= u64::from(byte) << self.n;
            self.n += 8;
        }
        (self.acc & (LUT as u64 - 1)) as usize
    }

    fn skip(&mut self, bits: u32) {
        self.acc >>= bits;
        self.n -= bits;
        self.used += bits as usize;
    }

    fn bytes_used(&self) -> usize {
        self.used.div_ceil(8)
    }
}

/// Decode a type-2 array body into `n` bytes.
pub(super) fn decode(src: &[u8], n: usize) -> Result<Vec<u8>> {
    let mut r = BitReader::new(src, 0, src.len(), false);
    if r.get(1) != 0 {
        return format_err("kraken: unsupported Huffman table form");
    }
    // Symbols by code length, in table order.
    let mut by_len: [Vec<u8>; MAX_LEN as usize + 1] = Default::default();
    if r.get(1) == 1 {
        let fb = r.get(2);
        let limit = 20 >> fb;
        let mut skip = r.get(1) == 1;
        let (mut sym, mut avg, mut count) = (0usize, 32i32, 0usize);
        while sym != 256 {
            if skip {
                skip = false;
            } else {
                sym += get_gamma(&mut r)?;
                if sym >= 256 {
                    break;
                }
            }
            let run = get_gamma(&mut r)?;
            if sym + run > 256 {
                return format_err("kraken: Huffman table overruns");
            }
            count += run;
            for _ in 0..run {
                let lz = r.bits.leading_zeros();
                if lz > limit {
                    return format_err("kraken: bad Huffman code length");
                }
                r.get(lz + 1);
                let v = (lz << fb | r.get(fb)) as i32;
                let l = (-(v & 1) ^ (v >> 1)) + ((avg + 2) >> 2);
                if !(1..=MAX_LEN as i32).contains(&l) {
                    return format_err("kraken: bad Huffman code length");
                }
                avg = l + ((3 * avg + 2) >> 2);
                by_len[l as usize].push(sym as u8);
                sym += 1;
            }
        }
        if sym != 256 || count < 2 {
            return format_err("kraken: bad Huffman table");
        }
    } else {
        let count = r.get(8);
        if count == 0 {
            return format_err("kraken: empty Huffman table");
        }
        if count == 1 {
            return Ok(vec![r.get(8) as u8; n]);
        }
        let cb = r.get(3);
        if cb > 4 {
            return format_err("kraken: bad Huffman table");
        }
        for _ in 0..count {
            let s = r.get(8) as u8;
            let l = r.get(cb) as usize + 1;
            by_len[l].push(s);
        }
    }
    // The decoder's table: each code fills 2^(11 − len) slots, indexed by the reversed bits.
    let mut lut = vec![(0u8, 0u32); LUT];
    let mut slot = 0usize;
    for (l, syms) in by_len.iter().enumerate().skip(1) {
        let step = 1usize << (MAX_LEN as usize - l);
        for &s in syms {
            if slot + step > LUT {
                return format_err("kraken: oversubscribed Huffman code");
            }
            for k in slot..slot + step {
                lut[(k as u32).reverse_bits() as usize >> (32 - MAX_LEN)] = (s, l as u32);
            }
            slot += step;
        }
    }
    if slot != LUT {
        return format_err("kraken: incomplete Huffman code");
    }

    let start = r.seam() as usize;
    if start + 3 > src.len() {
        return format_err("kraken: truncated Huffman array");
    }
    let split = usize::from(u16::from_le_bytes([src[start], src[start + 1]]));
    let a0 = start + 2;
    if a0 + split > src.len() {
        return format_err("kraken: bad Huffman split");
    }
    let mut streams = [
        LsbReader::new(src, a0, a0 + split, false),
        LsbReader::new(src, a0 + split, src.len(), true),
        LsbReader::new(src, a0 + split, src.len(), false),
    ];
    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        let s = &mut streams[i % 3];
        let (sym, l) = lut[s.peek()];
        s.skip(l);
        out.push(sym);
    }
    let [a, b, c] = &streams;
    if a.bytes_used() != split || b.bytes_used() + c.bytes_used() != src.len() - a0 - split {
        return format_err("kraken: Huffman streams do not meet");
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roundtrip(data: &[u8]) -> Option<usize> {
        let body = encode(data)?;
        assert_eq!(decode(&body, data.len()).unwrap(), data);
        Some(body.len())
    }

    #[test]
    fn skewed_bytes_shrink_and_round_trip() {
        let data: Vec<u8> = (0..50_000u32)
            .map(|i| b"abacaade"[(i * 7 % 13 % 8) as usize])
            .collect();
        assert!(roundtrip(&data).unwrap() < data.len() / 3);
    }

    #[test]
    fn edge_alphabets_round_trip() {
        // One symbol (with its never-used partner), both ends of the byte range, all 256.
        roundtrip(&[7u8; MIN_ARRAY]).unwrap();
        roundtrip(&[0u8; MIN_ARRAY]).unwrap();
        let mut d = vec![0u8; 300];
        d[150] = 255;
        roundtrip(&d);
        let all: Vec<u8> = (0..4096u32).map(|i| (i * 31 % 256) as u8).collect();
        roundtrip(&all);
        for n in MIN_ARRAY..MIN_ARRAY + 8 {
            roundtrip(&(0..n).map(|i| (i % 5) as u8).collect::<Vec<_>>()).unwrap();
        }
    }

    /// The console froze on Huffman arrays of every size and played with only those of 256 bytes
    /// and up, so nothing smaller is ever coded, however well it would shrink.
    #[test]
    fn arrays_under_the_minimum_stay_raw() {
        assert!(encode(&[0u8; MIN_ARRAY - 1]).is_none());
        assert!(encode(&[0u8; MIN_ARRAY]).is_some());
        let mut out = Vec::new();
        super::super::put_array(&mut out, &[0u8; MIN_ARRAY - 1]);
        assert_eq!(out[0] >> 4, 0, "raw array type");
    }

    /// A Fibonacci-like distribution wants codes far deeper than 11 bits.
    #[test]
    fn deep_trees_are_limited_to_eleven_bits() {
        let mut data = Vec::new();
        let (mut a, mut b) = (1usize, 1usize);
        for s in 0..24u8 {
            data.extend(std::iter::repeat_n(s, a));
            (a, b) = (b, a + b);
        }
        let mut freq = [0u32; 256];
        for &x in &data {
            freq[x as usize] += 1;
        }
        assert!(code_lengths(&freq).iter().all(|&l| l <= 11));
        roundtrip(&data);
    }

    /// The table read back independently: the decoder's own zigzag and average, so a delta
    /// written wrong cannot pass by being read back just as wrong.
    #[test]
    fn table_lengths_decode_by_the_reference_rule() {
        let mut lens = [0u8; 256];
        for (s, l) in [(3, 1), (4, 3), (9, 3), (200, 3), (201, 4), (255, 4)] {
            lens[s] = l;
        }
        for fb in 0..4 {
            let t = table(&lens, fb).unwrap();
            let mut r = BitReader::new(&t, 0, t.len(), false);
            assert_eq!((r.get(1), r.get(1), r.get(2), r.get(1)), (0, 1, fb, 0));
            let (mut sym, mut avg) = (0usize, 32i32);
            while sym < 256 {
                sym += get_gamma(&mut r).unwrap();
                if sym >= 256 {
                    break;
                }
                for _ in 0..get_gamma(&mut r).unwrap() {
                    let lz = r.bits.leading_zeros();
                    let v = r.get(lz + fb + 1) as i32 + ((lz as i32 - 1) << fb);
                    let codelen = (-(v & 1) ^ (v >> 1)) + ((avg + 2) >> 2);
                    avg = codelen + ((3 * avg + 2) >> 2);
                    assert_eq!(codelen, i32::from(lens[sym]), "fb {fb} sym {sym}");
                    sym += 1;
                }
            }
            assert_eq!(sym, 256);
        }
    }
}
