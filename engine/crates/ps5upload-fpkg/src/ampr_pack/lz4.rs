//! Raw LZ4 blocks (no frame), the codec `ampr_emu` decodes with `LZ4_decompress_safe`.
//!
//! `fast` is lz4_flex's encoder. `hc` is a hash-chain encoder written here, in the spirit of
//! LZ4HC: at every position it walks a chain of earlier positions with the same four-byte
//! prefix (more of them at higher levels), takes the longest match, and looks one byte ahead
//! before committing to it. Both obey the block format's end rules, which the decoder relies
//! on: the last five bytes are literals and no match starts in the last twelve.

const MIN_MATCH: usize = 4;
const LAST_LITERALS: usize = 5;
const MF_LIMIT: usize = 12;
const MAX_DISTANCE: usize = 65535;
const HASH_LOG: u32 = 16;
const WINDOW: usize = 1 << 16;

/// How a block is compressed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Fast,
    /// Hash-chain search, level 1 to 12: each level doubles the chain positions tried.
    High(u8),
}

pub fn compress(data: &[u8], mode: Mode) -> Vec<u8> {
    match mode {
        Mode::Fast => lz4_flex::block::compress(data),
        Mode::High(level) => compress_hc(data, level.clamp(1, 12)),
    }
}

/// Exactly `raw_size` bytes out of `block`, or an error for anything malformed.
pub fn decompress(block: &[u8], raw_size: usize) -> crate::Result<Vec<u8>> {
    let mut out = vec![0u8; raw_size];
    match lz4_flex::block::decompress_into(block, &mut out) {
        Ok(n) if n == raw_size => Ok(out),
        Ok(n) => crate::format_err(format!("an LZ4 block decoded to {n} of {raw_size} bytes")),
        Err(e) => crate::format_err(format!("an LZ4 block does not decode: {e}")),
    }
}

fn read32(d: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(d[at..at + 4].try_into().unwrap())
}

fn hash4(v: u32) -> usize {
    (v.wrapping_mul(2_654_435_761) >> (32 - HASH_LOG)) as usize
}

struct Chains {
    head: Vec<u32>,
    prev: Vec<u16>,
    next_insert: usize,
}

impl Chains {
    fn new() -> Self {
        Self {
            // Positions are stored + 1 so 0 means "none".
            head: vec![0; 1 << HASH_LOG],
            prev: vec![0; WINDOW],
            next_insert: 0,
        }
    }

    /// Thread every position before `upto` into its chain.
    fn insert_until(&mut self, d: &[u8], upto: usize) {
        while self.next_insert < upto {
            let p = self.next_insert;
            let h = hash4(read32(d, p));
            let last = self.head[h] as usize;
            let delta = if last == 0 { 0 } else { p + 1 - last };
            self.prev[p & (WINDOW - 1)] = if delta > MAX_DISTANCE {
                0
            } else {
                delta as u16
            };
            self.head[h] = (p + 1) as u32;
            self.next_insert += 1;
        }
    }

    /// The longest match for `p` (length, distance), walking at most `attempts` candidates;
    /// matches may run up to `limit`.
    fn best(&mut self, d: &[u8], p: usize, limit: usize, attempts: usize) -> (usize, usize) {
        self.insert_until(d, p);
        let want = read32(d, p);
        let mut cand = self.head[hash4(want)] as usize;
        let (mut best_len, mut best_dist) = (0, 0);
        let mut tries = attempts;
        while cand != 0 && tries > 0 {
            let c = cand - 1;
            if p - c > MAX_DISTANCE {
                break;
            }
            tries -= 1;
            if read32(d, c) == want && d.get(c + best_len) == d.get(p + best_len) {
                let mut len = MIN_MATCH;
                while p + len < limit && d[c + len] == d[p + len] {
                    len += 1;
                }
                if len > best_len {
                    best_len = len;
                    best_dist = p - c;
                    if p + len >= limit {
                        break; // nothing can be longer
                    }
                }
            }
            let delta = self.prev[c & (WINDOW - 1)] as usize;
            if delta == 0 || delta > c {
                break;
            }
            cand = c + 1 - delta;
        }
        (best_len, best_dist)
    }
}

fn put_len(out: &mut Vec<u8>, mut extra: usize) {
    while extra >= 255 {
        out.push(255);
        extra -= 255;
    }
    out.push(extra as u8);
}

fn emit(out: &mut Vec<u8>, literals: &[u8], match_len: usize, distance: usize) {
    let lit = literals.len();
    let ml = match_len - MIN_MATCH;
    let token = ((lit.min(15) as u8) << 4) | ml.min(15) as u8;
    out.push(token);
    if lit >= 15 {
        put_len(out, lit - 15);
    }
    out.extend_from_slice(literals);
    out.extend_from_slice(&(distance as u16).to_le_bytes());
    if ml >= 15 {
        put_len(out, ml - 15);
    }
}

fn emit_last(out: &mut Vec<u8>, literals: &[u8]) {
    let lit = literals.len();
    out.push((lit.min(15) as u8) << 4);
    if lit >= 15 {
        put_len(out, lit - 15);
    }
    out.extend_from_slice(literals);
}

fn compress_hc(d: &[u8], level: u8) -> Vec<u8> {
    let mut out = Vec::with_capacity(d.len() / 2 + 16);
    if d.len() < MF_LIMIT + 1 {
        emit_last(&mut out, d);
        return out;
    }
    let attempts = 1usize << (level - 1);
    // No match may start at or after `last_start`, nor run past `limit`.
    let last_start = d.len() - MF_LIMIT;
    let limit = d.len() - LAST_LITERALS;
    let mut chains = Chains::new();
    let mut anchor = 0;
    let mut p = 0;
    while p < last_start {
        let (len, dist) = chains.best(d, p, limit, attempts);
        if len < MIN_MATCH {
            p += 1;
            continue;
        }
        // One step of lazy evaluation: a longer match one byte on is worth a literal.
        if p + 1 < last_start {
            let (next_len, next_dist) = chains.best(d, p + 1, limit, attempts);
            if next_len > len + 1 {
                p += 1;
                emit(&mut out, &d[anchor..p], next_len, next_dist);
                p += next_len;
                anchor = p;
                continue;
            }
        }
        emit(&mut out, &d[anchor..p], len, dist);
        p += len;
        anchor = p;
    }
    emit_last(&mut out, &d[anchor..]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn samples() -> Vec<Vec<u8>> {
        let mut text = Vec::new();
        for i in 0..4000u32 {
            text.extend_from_slice(
                format!("line {} of some repetitive text {}\n", i % 97, i % 7).as_bytes(),
            );
        }
        let mut noise = Vec::with_capacity(70_000);
        let mut x = 0x1234_5678u32;
        for _ in 0..70_000 {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            noise.push(x as u8);
        }
        let mut mixed = noise[..30_000].to_vec();
        mixed.extend(vec![0u8; 40_000]);
        mixed.extend_from_slice(&text[..20_000]);
        vec![
            Vec::new(),
            vec![7],
            b"short".to_vec(),
            vec![0u8; 13],
            vec![0u8; 65536],
            text,
            noise,
            mixed,
            (0..65536u32).map(|i| (i / 3) as u8).collect(),
        ]
    }

    /// Every level's output decodes, through lz4_flex's safe decoder, to the input exactly,
    /// and compressible data shrinks.
    #[test]
    fn every_mode_round_trips() {
        for data in samples() {
            for mode in [
                Mode::Fast,
                Mode::High(1),
                Mode::High(4),
                Mode::High(9),
                Mode::High(12),
            ] {
                let c = compress(&data, mode);
                assert_eq!(
                    decompress(&c, data.len()).unwrap(),
                    data,
                    "{mode:?} len {}",
                    data.len()
                );
                if data.len() == 65536 && data[0] == 0 && data[65535] == 0 {
                    assert!(c.len() < 600, "{mode:?}: zeros took {} bytes", c.len());
                }
            }
        }
    }

    /// The end rules: the last sequence is literals only and at least five bytes long (for any
    /// input long enough to have matched at all).
    #[test]
    fn the_block_ends_in_literals() {
        let data = vec![0xAAu8; 1000];
        let c = compress(&data, Mode::High(9));
        // Walk the sequences and look at the last one.
        let mut at = 0;
        let mut last_literals = 0;
        while at < c.len() {
            let token = c[at];
            at += 1;
            let mut lit = (token >> 4) as usize;
            if lit == 15 {
                loop {
                    let b = c[at] as usize;
                    at += 1;
                    lit += b;
                    if b != 255 {
                        break;
                    }
                }
            }
            at += lit;
            if at >= c.len() {
                last_literals = lit;
                break;
            }
            at += 2;
            if token & 15 == 15 {
                while c[at] == 255 {
                    at += 1;
                }
                at += 1;
            }
        }
        assert!(last_literals >= LAST_LITERALS, "{last_literals}");
    }

    #[test]
    fn higher_levels_are_never_worse_on_text() {
        let text = &samples()[5];
        let fast = compress(text, Mode::Fast).len();
        let high = compress(text, Mode::High(9)).len();
        assert!(high <= fast, "hc {high} vs fast {fast}");
    }

    #[test]
    fn malformed_blocks_are_errors() {
        let c = compress(&samples()[5], Mode::High(9));
        assert!(decompress(&c, 10).is_err());
        assert!(decompress(&c[..c.len() / 2], samples()[5].len()).is_err());
    }
}
