//! The PS5 build's global allocator, ported from PS5 Dump Forge
//! (`crates/ps5-dump-forge-ps5/src/lib.rs`, github.com/quer3q/ps5-dump-forge, GPL-3.0-or-later).
//! `main.rs` installs [`Pool`] when built for the PS5; no other target uses it.
//!
//! [`Pool`] keeps freed blocks of 64 B to 4 MiB in power-of-two classes and hands them out again
//! instead of returning them to the libc (smaller ones stay with it). The PS5 libc serializes
//! every malloc, realloc and free across threads. Forge measured, on a console, 4 compression
//! threads at 12.7 MB/s on a game image (one thread: 11.7) with 25,000 libc calls per 32 MiB, and
//! 49.3 MB/s through this pool.

use std::alloc::{GlobalAlloc, Layout, System};
use std::ptr::null_mut;
use std::sync::atomic::{AtomicPtr, Ordering};

const MIN_SHIFT: u32 = 6;
const MAX_SHIFT: u32 = 22;
const CLASSES: usize = (MAX_SHIFT - MIN_SHIFT + 1) as usize;
const SLOTS: usize = 16;

/// Free blocks per class, lock-free: a slot holds one or is null.
static FREE: [[AtomicPtr<u8>; SLOTS]; CLASSES] =
    [const { [const { AtomicPtr::new(null_mut()) }; SLOTS] }; CLASSES];

/// Slots a class uses: all up to 1 MiB, 4 above, so at most ~56 MiB is kept. Blocks round up to
/// a power of two, so a size just past one costs twice its bytes.
fn slots(c: usize) -> usize {
    if c as u32 + MIN_SHIFT <= 20 {
        SLOTS
    } else {
        4
    }
}

/// The class of `l`, for the sizes and alignments kept here. Blocks under 64 B stay with the
/// libc, as in the build Forge measured.
fn class(l: &Layout) -> Option<usize> {
    let size = l.size();
    if !(1 << MIN_SHIFT..=1 << MAX_SHIFT).contains(&size) || l.align() > 16 {
        return None;
    }
    Some((size.next_power_of_two().trailing_zeros() - MIN_SHIFT) as usize)
}

fn class_layout(c: usize) -> Layout {
    // SAFETY: a power of two from 64 B to 4 MiB, aligned to 16.
    unsafe { Layout::from_size_align_unchecked(1 << (c as u32 + MIN_SHIFT), 16) }
}

/// The PS5 build's allocator (see the module docs).
pub struct Pool;

// SAFETY: every block of a class is allocated from `System` at its class layout and returned
// to `System` with it; a slot hands a block to one caller only (`swap`).
unsafe impl GlobalAlloc for Pool {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        let Some(c) = class(&l) else {
            return unsafe { System.alloc(l) };
        };
        for slot in &FREE[c][..slots(c)] {
            if !slot.load(Ordering::Relaxed).is_null() {
                let p = slot.swap(null_mut(), Ordering::Acquire);
                if !p.is_null() {
                    return p;
                }
            }
        }
        unsafe { System.alloc(class_layout(c)) }
    }

    unsafe fn alloc_zeroed(&self, l: Layout) -> *mut u8 {
        if class(&l).is_none() {
            return unsafe { System.alloc_zeroed(l) };
        }
        let p = unsafe { self.alloc(l) };
        if !p.is_null() {
            unsafe { p.write_bytes(0, l.size()) };
        }
        p
    }

    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        let Some(c) = class(&l) else {
            return unsafe { System.dealloc(p, l) };
        };
        for slot in &FREE[c][..slots(c)] {
            if slot.load(Ordering::Relaxed).is_null()
                && slot
                    .compare_exchange(null_mut(), p, Ordering::Release, Ordering::Relaxed)
                    .is_ok()
            {
                return;
            }
        }
        unsafe { System.dealloc(p, class_layout(c)) }
    }

    unsafe fn realloc(&self, p: *mut u8, l: Layout, new: usize) -> *mut u8 {
        // SAFETY: `realloc`'s contract: `new` is non-zero and, rounded to `l.align()`, fits.
        let nl = unsafe { Layout::from_size_align_unchecked(new, l.align()) };
        match (class(&l), class(&nl)) {
            (None, None) => unsafe { System.realloc(p, l, new) },
            // The block already holds a whole class.
            (Some(a), Some(b)) if a == b => p,
            _ => {
                let q = unsafe { self.alloc(nl) };
                if !q.is_null() {
                    unsafe { std::ptr::copy_nonoverlapping(p, q, l.size().min(new)) };
                    unsafe { self.dealloc(p, l) };
                }
                q
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Kept blocks come back; a grow within a class keeps the block and its bytes, one past it
    /// moves them; a reused block is zeroed when asked; sizes past the classes and large
    /// alignments go to `System`.
    #[test]
    fn classes_reuse_grow_and_zero() {
        let pool = Pool;
        let l = |size| Layout::from_size_align(size, 8).unwrap();
        unsafe {
            // The 2 KiB class (1025..=2048 bytes): nothing else in this test binary uses `Pool`.
            let a = pool.alloc(l(1500));
            a.write_bytes(0xAB, 1500);
            let same = pool.realloc(a, l(1500), 2048);
            assert_eq!(same, a);
            assert_eq!(*same.add(1499), 0xAB);
            let moved = pool.realloc(same, l(2048), 2049);
            assert_ne!(moved, a);
            assert_eq!(*moved.add(1499), 0xAB);
            // `a`'s block was kept: the next 2 KiB block is it, zeroed when asked.
            let z = pool.alloc_zeroed(l(1100));
            assert_eq!(z, a);
            assert!((0..1100).all(|i| *z.add(i) == 0));
            pool.dealloc(z, l(1100));
            pool.dealloc(moved, l(2049));

            assert_eq!(class(&l(63)), None);
            assert_eq!(class(&l(64)), Some(0));
            assert_eq!(class(&l(65)), Some(1));
            assert_eq!(class(&l(4 << 20)), Some(CLASSES - 1));
            assert_eq!(class(&l((4 << 20) + 1)), None);
            assert_eq!(class(&Layout::from_size_align(64, 32).unwrap()), None);
            let big = pool.alloc_zeroed(l(5 << 20));
            assert_eq!(*big.add((5 << 20) - 1), 0);
            pool.dealloc(big, l(5 << 20));
        }
    }
}
