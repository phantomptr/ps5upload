//! Where a file's blocks go. Each file (or directory) owns one contiguous run of data
//! blocks, the "slots", filled in the order FreeBSD's `ffs_balloc` allocates them: the 12
//! direct blocks, then each indirect block right before the blocks it maps (pre-order).
//! So one start index per file fixes every pointer, and the writer can produce any
//! indirect block from arithmetic alone.

use crate::geom::NINDIR;

/// Direct block pointers in an inode (`UFS_NDADDR`).
pub const NDADDR: u64 = 12;
/// Indirect levels (`UFS_NIADDR`).
pub const NIADDR: usize = 3;

/// Data blocks one indirect tree of `level` maps: NINDIR^level.
pub fn span(level: u32) -> u64 {
    NINDIR.pow(level)
}

/// Slots of a *full* tree of `level` (indirect blocks included); level 0 is one data block.
fn full_slots(level: u32) -> u64 {
    (0..level).fold(1, |acc, _| 1 + NINDIR * acc)
}

/// Slots of a tree of `level` that maps its first `k` (1..=span) data blocks.
fn tree_slots(level: u32, k: u64) -> u64 {
    if level == 0 {
        return 1;
    }
    let per = span(level - 1);
    let full = k / per;
    let rem = k % per;
    1 + full * full_slots(level - 1)
        + if rem > 0 {
            tree_slots(level - 1, rem)
        } else {
            0
        }
}

/// Most data blocks an inode can address.
pub fn max_blocks() -> u64 {
    NDADDR + span(1) + span(2) + span(3)
}

/// All slots (data + indirect blocks) of a file of `n` data blocks; None past [`max_blocks`].
pub fn total_slots(n: u64) -> Option<u64> {
    if n > max_blocks() {
        return None;
    }
    let mut left = n;
    let direct = left.min(NDADDR);
    let mut total = direct;
    left -= direct;
    for level in 1..=NIADDR as u32 {
        if left == 0 {
            break;
        }
        let take = left.min(span(level));
        total += tree_slots(level, take);
        left -= take;
    }
    Some(total)
}

/// Slot of each top-level indirect block (`di_ib[0..3]`) of a file of `n` data blocks.
/// A tree exists only when every earlier one is full, hence the closed form.
pub fn indirect_roots(n: u64) -> [Option<u64>; NIADDR] {
    let mut out = [None; NIADDR];
    let mut slot = NDADDR;
    let mut covered = NDADDR;
    for (i, root) in out.iter_mut().enumerate() {
        let level = i as u32 + 1;
        if n <= covered {
            break;
        }
        *root = Some(slot);
        slot += full_slots(level);
        covered += span(level);
    }
    out
}

/// One piece of a file's slot sequence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Seg {
    /// `count` data blocks, logical blocks `lbn..`, at slots `slot..`.
    Data { slot: u64, lbn: u64, count: u64 },
    /// An indirect block of `level` at `slot`, mapping the next `k` data blocks.
    Indirect { slot: u64, level: u32, k: u64 },
}

/// The slot sequence of a file of `n` data blocks, in allocation (= disk) order.
/// ponytail: built whole per file; a full double-indirect tree is 16K entries, a
/// triple-indirect file (> 4.4 TB) would need ~128 MiB here.
pub fn segments(n: u64) -> Vec<Seg> {
    let mut out = Vec::new();
    let direct = n.min(NDADDR);
    if direct > 0 {
        out.push(Seg::Data {
            slot: 0,
            lbn: 0,
            count: direct,
        });
    }
    let (mut slot, mut lbn, mut left) = (direct, direct, n - direct);
    for level in 1..=NIADDR as u32 {
        if left == 0 {
            break;
        }
        let take = left.min(span(level));
        tree(level, take, &mut slot, &mut lbn, &mut out);
        left -= take;
    }
    out
}

fn tree(level: u32, k: u64, slot: &mut u64, lbn: &mut u64, out: &mut Vec<Seg>) {
    out.push(Seg::Indirect {
        slot: *slot,
        level,
        k,
    });
    *slot += 1;
    if level == 1 {
        out.push(Seg::Data {
            slot: *slot,
            lbn: *lbn,
            count: k,
        });
        *slot += k;
        *lbn += k;
        return;
    }
    let per = span(level - 1);
    let mut left = k;
    while left > 0 {
        let t = left.min(per);
        tree(level - 1, t, slot, lbn, out);
        left -= t;
    }
}

/// Slots of the children of the indirect block at `slot` (level, mapping k blocks): child
/// i starts right after its earlier siblings, which are all full trees.
pub fn children(slot: u64, level: u32, k: u64) -> impl Iterator<Item = u64> {
    let per = span(level - 1);
    let step = full_slots(level - 1);
    (0..k.div_ceil(per)).map(move |i| slot + 1 + i * step)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn count(segs: &[Seg]) -> u64 {
        segs.iter()
            .map(|s| match s {
                Seg::Data { count, .. } => *count,
                Seg::Indirect { .. } => 1,
            })
            .sum()
    }

    /// Slots are contiguous and gap-free, and the closed forms agree with the walk, at
    /// every transition: direct -> single -> double -> triple.
    #[test]
    fn transitions() {
        let s1 = NDADDR + span(1);
        let s2 = s1 + span(2);
        for n in [
            0,
            1,
            11,
            12,
            13,
            s1 - 1,
            s1,
            s1 + 1,
            s1 + NINDIR,
            s1 + NINDIR + 1,
            s2 - 1,
            s2,
            s2 + 1,
        ] {
            let segs = segments(n);
            assert_eq!(count(&segs), total_slots(n).unwrap(), "n={n}");
            let mut next = 0;
            let mut next_lbn = 0;
            for s in &segs {
                match *s {
                    Seg::Data { slot, lbn, count } => {
                        assert_eq!((slot, lbn), (next, next_lbn));
                        next += count;
                        next_lbn += count;
                    }
                    Seg::Indirect { slot, .. } => {
                        assert_eq!(slot, next);
                        next += 1;
                    }
                }
            }
            assert_eq!(next_lbn, n);
            // Top-level roots are the first Indirect segments of each level in order.
            let roots = indirect_roots(n);
            let mut tops = segs.iter().filter_map(|s| match *s {
                Seg::Indirect { slot, level, .. } => Some((slot, level)),
                _ => None,
            });
            for (i, r) in roots.iter().enumerate() {
                if let Some(r) = r {
                    let first = tops.find(|&(_, l)| l == i as u32 + 1).unwrap();
                    assert_eq!(first.0, *r, "n={n} level {}", i + 1);
                }
            }
        }
        assert_eq!(indirect_roots(12), [None, None, None]);
        assert_eq!(indirect_roots(13), [Some(12), None, None]);
        assert_eq!(
            indirect_roots(s1 + 1),
            [Some(12), Some(12 + 1 + NINDIR), None]
        );
        assert!(indirect_roots(s2 + 1)[2].is_some());
        assert_eq!(total_slots(max_blocks() + 1), None);
    }

    /// Each indirect block's children are exactly the next-level indirects / data runs
    /// that the walk emits under it.
    #[test]
    fn children_point_at_the_walk() {
        let n = NDADDR + span(1) + 3 * NINDIR + 5;
        let segs = segments(n);
        for (i, s) in segs.iter().enumerate() {
            if let Seg::Indirect { slot, level, k } = *s {
                let kids: Vec<u64> = children(slot, level, k).collect();
                assert_eq!(kids.len() as u64, k.div_ceil(span(level - 1)));
                if level == 1 {
                    let Seg::Data { slot: d, count, .. } = segs[i + 1] else {
                        panic!()
                    };
                    assert_eq!(kids, (d..d + count).collect::<Vec<_>>());
                } else {
                    let below: Vec<u64> = segs[i + 1..]
                        .iter()
                        .filter_map(|s| match *s {
                            Seg::Indirect { slot, level: l, .. } if l == level - 1 => Some(slot),
                            _ => None,
                        })
                        .take(kids.len())
                        .collect();
                    assert_eq!(kids, below);
                }
            }
        }
    }
}
