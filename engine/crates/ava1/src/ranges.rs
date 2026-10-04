//! Which bytes of a file, and which files of a job, are already durable (SPEC.md §14).
use std::collections::{BTreeMap, BTreeSet};

use crate::gen::{self, FileRange, FileRun};

/// Sorted, disjoint, merged half-open ranges.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RangeSet {
    v: Vec<(u64, u64)>,
}

impl RangeSet {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert(&mut self, start: u64, end: u64) {
        if start >= end {
            return;
        }
        let (mut s, mut e) = (start, end);
        // First range that ends at or after `start` (touching ranges merge).
        let i = self.v.partition_point(|r| r.1 < s);
        let mut j = i;
        while j < self.v.len() && self.v[j].0 <= e {
            s = s.min(self.v[j].0);
            e = e.max(self.v[j].1);
            j += 1;
        }
        self.v.splice(i..j, [(s, e)]);
    }

    pub fn iter(&self) -> impl Iterator<Item = (u64, u64)> + '_ {
        self.v.iter().copied()
    }

    pub fn covered(&self) -> u64 {
        self.v.iter().map(|r| r.1 - r.0).sum()
    }

    pub fn covers(&self, start: u64, end: u64) -> bool {
        let i = self.v.partition_point(|r| r.1 <= start);
        self.v.get(i).is_some_and(|r| r.0 <= start && end <= r.1)
    }

    pub fn is_full(&self, total: u64) -> bool {
        total == 0 || self.covers(0, total)
    }

    pub fn missing(&self, total: u64) -> Vec<(u64, u64)> {
        let mut out = Vec::new();
        let mut at = 0;
        for &(s, e) in &self.v {
            if s > at {
                out.push((at, s.min(total)));
            }
            at = at.max(e);
            if at >= total {
                break;
            }
        }
        if at < total {
            out.push((at, total));
        }
        out.retain(|r| r.0 < r.1);
        out
    }
}

pub fn runs(set: &BTreeSet<u32>) -> Vec<FileRun> {
    let mut out: Vec<FileRun> = Vec::new();
    for &i in set {
        match out.last_mut() {
            Some(r) if r.first + r.count == i => r.count += 1,
            _ => out.push(FileRun { first: i, count: 1 }),
        }
    }
    out
}

/// The most file ids any one list of runs may name (16 Mi: a game is ~223 k). Longer is hostile or corrupt.
pub const MAX_RUN_IDS: u64 = 1 << 24;

/// Whether the runs name no more than `MAX_RUN_IDS` ids in all (a journal record that names more is refused).
pub fn runs_within_limit(runs: &[FileRun]) -> bool {
    runs.iter().map(|r| r.count as u64).sum::<u64>() <= MAX_RUN_IDS
}

pub fn from_runs(runs: &[FileRun]) -> BTreeSet<u32> {
    runs.iter()
        .flat_map(|r| r.first..r.first.saturating_add(r.count))
        .take(MAX_RUN_IDS as usize)
        .collect()
}

/// A receiver's answer to "what do you have": done files and durable ranges of the rest.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Need {
    pub done: BTreeSet<u32>,
    pub partial: BTreeMap<u32, RangeSet>,
    /// Bytes the receiver's drive already holds for the job's unfinished large files (the
    /// allocated blocks of their part files; the JobMap `held` extension on its last page).
    /// 0 when the receiver does not say: nothing is credited that was not reported.
    pub held: u64,
}

/// Items per map page. An item is `u32le(len) ‖ fields` (SPEC §3): 12 bytes for a run,
/// 24 for a range, so a full page is at most 48 KiB — inside `manifest::PAGE_BYTES`
/// (60 KiB), the same page budget the manifest uses. Pinned by
/// `a_full_page_of_the_widest_items_fits`.
pub const MAP_PAGE_ITEMS: usize = 2000;

impl Need {
    /// One or more JobMap pages; the last has `last = 1`.
    pub fn to_pages(&self, job_id: [u8; 16], status: u16) -> Vec<gen::JobMap> {
        let all_runs = runs(&self.done);
        let all_ranges: Vec<FileRange> = self
            .partial
            .iter()
            .flat_map(|(f, r)| {
                r.iter().map(move |(s, e)| FileRange {
                    file_id: *f,
                    offset: s,
                    len: e - s,
                })
            })
            .collect();
        let mut pages = Vec::new();
        let (mut ri, mut gi) = (0, 0);
        loop {
            let mut budget = MAP_PAGE_ITEMS;
            let done: Vec<FileRun> = all_runs[ri..].iter().take(budget).cloned().collect();
            ri += done.len();
            budget -= done.len();
            let partial: Vec<FileRange> = all_ranges[gi..].iter().take(budget).cloned().collect();
            gi += partial.len();
            let last = ri == all_runs.len() && gi == all_ranges.len();
            pages.push(gen::JobMap {
                held: (last && self.held > 0).then_some(self.held),
                job_id,
                status,
                last: u8::from(last),
                done,
                partial,
                message: None,
            });
            if last {
                return pages;
            }
        }
    }

    pub fn add_page(&mut self, m: &gen::JobMap) {
        if let Some(h) = m.held {
            self.held = self.held.max(h);
        }
        self.done.extend(from_runs(&m.done));
        for r in &m.partial {
            self.partial
                .entry(r.file_id)
                .or_default()
                // `len` is peer-supplied: saturate exactly as `from_runs` does, so a
                // bogus range can never panic or wrap.
                .insert(r.offset, r.offset.saturating_add(r.len));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_full_page_of_the_widest_items_fits() {
        // The existing large-map test fills pages with runs (12 bytes an item); a page
        // filled with FileRanges is the widest case (24 bytes an item) and is never
        // reached there, since `partial` stays empty. Encode one for real against the
        // same 60 KiB page budget the manifest test pins.
        let mut need = Need::default();
        for i in 0..MAP_PAGE_ITEMS as u32 {
            need.done.insert(i);
            need.partial.entry(i).or_default().insert(0, 1 << 20);
        }
        let pages = need.to_pages([0; 16], 0);
        assert_eq!(
            pages.len(),
            2,
            "2000 runs + 2000 ranges = exactly two pages"
        );
        for (i, p) in pages.iter().enumerate() {
            let n = crate::wire::Message::to_bytes(p).unwrap().len();
            assert!(n <= crate::manifest::PAGE_BYTES, "page {i} is {n} bytes");
        }
        assert_eq!(pages[0].last, 0);
        assert_eq!(pages[1].last, 1);
    }

    #[test]
    fn held_rides_the_last_page_only_and_a_map_without_it_credits_nothing() {
        let mut need = Need::default();
        for i in 0..MAP_PAGE_ITEMS as u32 + 1 {
            need.done.insert(i * 2); // runs of one: enough items for a second page
        }
        need.held = 7 << 30;
        let pages = need.to_pages([0; 16], 0);
        assert!(pages.len() >= 2);
        assert!(pages[..pages.len() - 1].iter().all(|p| p.held.is_none()));
        assert_eq!(pages.last().unwrap().held, Some(7 << 30));
        let mut got = Need::default();
        for p in &pages {
            got.add_page(p);
        }
        assert_eq!(got.held, 7 << 30);
        // A receiver that never says: 0, and a zero is not sent.
        let none = Need::default().to_pages([0; 16], 0);
        assert_eq!(none[0].held, None);
        let mut absent = Need::default();
        absent.add_page(&none[0]);
        assert_eq!(absent.held, 0);
    }

    #[test]
    fn inserts_merge_and_report_coverage() {
        let mut r = RangeSet::new();
        r.insert(10, 20);
        r.insert(30, 40);
        r.insert(20, 30); // bridges both
        assert_eq!(r.iter().collect::<Vec<_>>(), vec![(10, 40)]);
        r.insert(0, 5);
        r.insert(3, 12);
        assert_eq!(r.iter().collect::<Vec<_>>(), vec![(0, 40)]);
        r.insert(50, 60);
        assert_eq!(r.covered(), 50);
        assert!(r.covers(0, 40) && r.covers(52, 58) && !r.covers(39, 51));
        assert_eq!(r.missing(70), vec![(40, 50), (60, 70)]);
        assert!(!r.is_full(60));
        r.insert(40, 50);
        assert!(r.is_full(60));
    }

    #[test]
    fn runs_round_trip() {
        let s: BTreeSet<u32> = [0, 1, 2, 5, 7, 8, 100].into_iter().collect();
        let r = runs(&s);
        assert_eq!(
            r.iter().map(|x| (x.first, x.count)).collect::<Vec<_>>(),
            vec![(0, 3), (5, 1), (7, 2), (100, 1)]
        );
        assert_eq!(from_runs(&r), s);
    }

    #[test]
    fn a_need_round_trips_through_the_wire() {
        let mut n = Need::default();
        n.done.extend([0, 1, 4]);
        n.partial.entry(2).or_default().insert(0, 1 << 20);
        n.partial.entry(2).or_default().insert(3 << 20, 4 << 20);
        let pages = n.to_pages([1; 16], crate::gen::STATUS_OK);
        assert_eq!(pages.len(), 1);
        assert_eq!(pages[0].last, 1);
        let mut back = Need::default();
        back.add_page(&pages[0]);
        assert_eq!(back, n);
    }

    #[test]
    fn a_large_map_is_paged_under_the_control_cap() {
        use crate::wire::Message;
        let mut n = Need::default();
        n.done.extend((0..300_000u32).step_by(2)); // 150k runs: the worst case
        let pages = n.to_pages([1; 16], crate::gen::STATUS_OK);
        assert!(pages.len() > 1);
        let mut back = Need::default();
        for (i, p) in pages.iter().enumerate() {
            assert!(p.to_bytes().unwrap().len() <= crate::manifest::PAGE_BYTES);
            assert_eq!(p.last, u8::from(i + 1 == pages.len()));
            back.add_page(p);
        }
        assert_eq!(back, n);
    }
}
