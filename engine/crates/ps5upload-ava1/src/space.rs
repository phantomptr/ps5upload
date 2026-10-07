//! The up-front free-space check for an upload (design 015/02, review 019 F1/F2).
//!
//! Once the receiver has said what it already has (the JobMap), the sender knows exactly what
//! is left to put on the drive: the unfinished files, less the blocks the console's part files
//! already hold. This module decides whether that fits the destination volume, and counts
//! what other uploads running in this engine have promised and not written yet, so two jobs
//! cannot both be admitted into room only one of them can use.
//!
//! It refuses only what cannot fit: the room is the volume's free space less a small working
//! margin (`Volume::allocatable_bytes`), and the credit is only what the console reported.
//! Internal storage also holds back about a fifth more as data is written
//! (`ps5upload_core::volumes::internal_write_cost`, measured); that is shown to the user as
//! "may not fit" before the upload and named in the message if the console does run out, but
//! it never refuses an upload, because the figure is an estimate. A console that cannot be asked (an old payload, a busy
//! management port) is not a refusal: the transfer's own ENOSPC still ends it.
use std::collections::HashMap;
use std::io::Write;
use std::sync::{Arc, Mutex, OnceLock};

use ava1::send::{SpaceFigures, SpaceGate};

use crate::pool::host_of;

/// What the destination's volume offers right now.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Room {
    /// What the message calls the drive: its mount path (`/data`, `/mnt/ext0`) or the
    /// destination asked about.
    pub volume: String,
    /// The drive's device id when the console said it: uploads to different folders of one drive
    /// then share a ledger entry. Without it the volume name is the key.
    pub dev: Option<u64>,
    pub free_bytes: u64,
    pub reserve_bytes: u64,
    /// Free space less the reserve and the learned gap: what an upload may fill.
    pub allocatable_bytes: u64,
    /// The drive's total size (0 = unknown).
    pub total_bytes: u64,
}

/// Asks the console for the room under `dest`. `None` = unknown (never a refusal).
pub type RoomProbe = Arc<dyn Fn(&str, &str) -> Option<Room> + Send + Sync>;

/// The address management calls take for a console given as a host (a stale `host:port` loses
/// its port).
pub(crate) fn mgmt_addr(console: &str) -> String {
    host_of(console)
}

/// The real probe: `fs.freespace` (usable room on the drive holding `dest`, the console's own
/// post-reserve figure); a payload without it is asked `fs.volumes` instead and the same margin is
/// applied here. Either answer is "free space less a small working margin": nothing is guessed.
pub fn volumes_probe() -> RoomProbe {
    Arc::new(|console, dest| {
        let host = mgmt_addr(console);
        match ps5upload_core::volumes::free_space(&host, dest) {
            Ok(f) => {
                return Some(Room {
                    volume: dest.to_string(),
                    dev: Some(f.dev),
                    free_bytes: f.free_bytes,
                    reserve_bytes: f.reserve_bytes,
                    allocatable_bytes: f.usable_bytes,
                    total_bytes: f.total_bytes,
                })
            }
            Err(e) => {
                let _ = writeln!(
                    std::io::stderr(),
                    "ava1: fs.freespace unavailable on {console}: {e:#}; trying fs.volumes"
                );
            }
        }
        let list = match ps5upload_core::volumes::list_volumes(&host) {
            Ok(l) => l,
            Err(e) => {
                let _ = writeln!(
                    std::io::stderr(),
                    "ava1: space check unavailable for {console}: {e:#}"
                );
                return None;
            }
        };
        let Some(v) = list.find_for_path(dest) else {
            let _ = writeln!(
                std::io::stderr(),
                "ava1: space check found no volume for {dest} on {console}"
            );
            return None;
        };
        Some(Room {
            volume: v.path.clone(),
            dev: None,
            free_bytes: v.free_bytes,
            reserve_bytes: v.safety_reserve_bytes(),
            allocatable_bytes: v.allocatable_bytes(),
            total_bytes: v.total_bytes,
        })
    })
}

/// One running upload's promise: the bytes it still has to allocate, and the room the volume
/// offered when it was admitted.
#[derive(Debug, Clone)]
struct Promise {
    key: (String, String),
    to_allocate: u64,
    room_then: u64,
    /// The figures the check was admitted on, for the message if the console's own allocator
    /// refuses later (see [`late_no_space_detail`]).
    volume: String,
    free_bytes: u64,
    reserve_bytes: u64,
}

impl Promise {
    /// What this job has still to take from the volume: its promise less what the volume has
    /// lost since it was admitted (that room is already gone from `room_now`, so counting it
    /// again would refuse an upload that fits). Losses from other causes make this smaller,
    /// never larger: the error is always towards admitting.
    fn outstanding(&self, room_now: u64) -> u64 {
        self.to_allocate
            .saturating_sub(self.room_then.saturating_sub(room_now))
    }
}

fn ledger() -> &'static Mutex<HashMap<[u8; 16], Promise>> {
    static L: OnceLock<Mutex<HashMap<[u8; 16], Promise>>> = OnceLock::new();
    L.get_or_init(Mutex::default)
}

/// Removes the job's promise when the upload ends, by any path.
pub(crate) struct Reservation {
    job: [u8; 16],
}

impl Reservation {
    pub(crate) fn new(job: [u8; 16]) -> Self {
        Self { job }
    }
}

impl Drop for Reservation {
    fn drop(&mut self) {
        ledger()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&self.job);
    }
}

/// A size a person reads: GiB from a gibibyte up, else MiB.
fn gib(n: u64) -> String {
    if n >= 1 << 30 {
        format!("{:.1} GiB", n as f64 / (1u64 << 30) as f64)
    } else {
        format!("{:.1} MiB", n as f64 / (1u64 << 20) as f64)
    }
}

/// The verdict for one job: `Ok` when what it must still allocate fits the room left after
/// the other jobs' promises. The message says how much is needed AFTER crediting the console's
/// copy, which is the number a person can act on.
pub fn judge(figures: &SpaceFigures, room: &Room, promised_elsewhere: u64) -> Result<(), String> {
    let need = figures.to_allocate();
    let avail = room.allocatable_bytes.saturating_sub(promised_elsewhere);
    if need <= avail {
        return Ok(());
    }
    let mut m = format!(
        "{} needs {} more bytes ({}) for this upload but only {} bytes ({}) are safely \
         allocatable ({} bytes free, {} reserved",
        room.volume,
        need,
        gib(need),
        avail,
        gib(avail),
        room.free_bytes,
        room.reserve_bytes
    );
    if promised_elsewhere > 0 {
        m.push_str(&format!(
            "; {} bytes are promised to other uploads running now",
            promised_elsewhere
        ));
    }
    m.push_str(&format!("); short by {} bytes.", need - avail));
    let kept = figures.job_bytes.saturating_sub(need);
    if kept > 0 {
        m.push_str(&format!(
            " {} bytes ({}) of this upload are already on the console and will be reused.",
            kept,
            gib(kept)
        ));
    }
    Err(m)
}

/// The sentence that explains a gap between what Volumes shows and what the console can allocate.
pub const POOL_SENTENCE: &str = "The PS5's internal storage holds back about a fifth more than is written, so less fits than its free space suggests.";

/// The refusal text for a console that ran out of room (ENOSPC while preparing a file, or on a
/// write) after the up-front check had admitted the job. Same figures as the check, plus
/// [`POOL_SENTENCE`]. Pure, so the wording is tested without a console.
pub fn pool_refusal(
    volume: &str,
    need: u64,
    avail: u64,
    free_bytes: u64,
    reserve_bytes: u64,
) -> String {
    format!(
        "{volume} needs {need} more bytes ({}) for this upload and {avail} bytes ({}) were \
         allocatable when it was checked ({free_bytes} bytes free, {reserve_bytes} reserved), but \
         the console ran out of room before the transfer could use it. {POOL_SENTENCE} \
         Nothing was lost: the partial upload is kept, so freeing space and retrying resumes it.",
        gib(need),
        gib(avail),
    )
}

/// The detail for an ENOSPC the console reported after admitting `job`, built from the figures it
/// was admitted on; without them (the room was never probed) the sentence alone, no invented numbers.
pub(crate) fn late_no_space_detail(job: &[u8; 16], console_message: &str) -> String {
    let l = ledger().lock().unwrap_or_else(|e| e.into_inner());
    match l.get(job) {
        Some(p) => pool_refusal(
            &p.volume,
            p.to_allocate,
            p.room_then,
            p.free_bytes,
            p.reserve_bytes,
        ),
        None => format!(
            "The console ran out of room for this upload ({console_message}). {POOL_SENTENCE} \
             The partial upload is kept, so freeing space and retrying resumes it."
        ),
    }
}

/// The gate an upload to `dest` on `console` runs once the receiver has answered. Runs on a
/// blocking thread (it asks the console over the management channel).
pub(crate) fn gate(probe: RoomProbe, console: String, dest: String, job: [u8; 16]) -> SpaceGate {
    Arc::new(move |figures: &SpaceFigures| {
        // The probe is a network call: never under the ledger's lock.
        let Some(room) = probe(&console, &dest) else {
            return Ok(());
        };
        let key = (
            host_of(&console),
            room.dev
                .map_or_else(|| room.volume.clone(), |d| format!("dev:{d}")),
        );
        let mut l = ledger().lock().unwrap_or_else(|e| e.into_inner());
        let elsewhere: u64 = l
            .iter()
            .filter(|(id, p)| **id != job && p.key == key)
            .map(|(_, p)| p.outstanding(room.allocatable_bytes))
            .fold(0u64, |a, n| a.saturating_add(n));
        match judge(figures, &room, elsewhere) {
            Ok(()) => {
                l.insert(
                    job,
                    Promise {
                        key,
                        to_allocate: figures.to_allocate(),
                        room_then: room.allocatable_bytes,
                        volume: room.volume.clone(),
                        free_bytes: room.free_bytes,
                        reserve_bytes: room.reserve_bytes,
                    },
                );
                Ok(())
            }
            Err(why) => {
                l.remove(&job);
                Err(why)
            }
        }
    })
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_volume_fallback_asks_by_host() {
        assert_eq!(super::mgmt_addr("192.168.86.100"), "192.168.86.100");
        assert_eq!(super::mgmt_addr("192.168.86.100:9020"), "192.168.86.100");
        assert_eq!(super::mgmt_addr("[fe80::1]:9020"), "[fe80::1]");
    }

    use super::*;

    #[test]
    fn a_late_enospc_reads_as_the_up_front_refusal_with_the_pool_sentence() {
        let job = [0x5a; 16];
        ledger().lock().unwrap().insert(
            job,
            Promise {
                key: ("h".into(), "v".into()),
                to_allocate: 164 * GB,
                room_then: 170 * GB,
                volume: "/data".into(),
                free_bytes: 171 * GB,
                reserve_bytes: GB,
            },
        );
        let d = late_no_space_detail(&job, "full");
        ledger().lock().unwrap().remove(&job);
        assert!(d.contains("/data needs "), "{d}");
        assert!(d.contains(&format!("needs {} more bytes", 164 * GB)), "{d}");
        assert!(d.contains(&format!("{} bytes free", 171 * GB)), "{d}");
        assert!(d.contains(POOL_SENTENCE), "{d}");
        assert!(d.contains("partial upload is kept"), "{d}");
        // Never admitted by a probe: the sentence, and no figures made up.
        let d = late_no_space_detail(&[0x5b; 16], "drive is full");
        assert!(d.contains(POOL_SENTENCE) && !d.contains("needs"), "{d}");
    }

    const GB: u64 = 1 << 30;

    fn room(free: u64) -> Room {
        Room {
            volume: "/data".into(),
            dev: None,
            free_bytes: free,
            reserve_bytes: GB,
            allocatable_bytes: free.saturating_sub(GB),
            ..Room::default()
        }
    }

    fn figures(job: u64, durable: u64, held: u64) -> SpaceFigures {
        SpaceFigures {
            job_bytes: job,
            durable_bytes: durable,
            held_bytes: held,
            unfinished_bytes: job - durable,
            files_total: 3,
            files_done: 1,
        }
    }

    #[test]
    fn a_resume_is_admitted_when_the_rest_fits_though_the_whole_would_not() {
        // #365: 280 GB folder, 270 GB already on the console, 10 GB of room.
        let f = figures(280 * GB, 270 * GB, 0);
        assert!(judge(&f, &room(11 * GB + GB / 2), 0).is_ok());
        // The same drive refuses a fresh 280 GB upload.
        let fresh = figures(280 * GB, 0, 0);
        assert!(judge(&fresh, &room(11 * GB + GB / 2), 0).is_err());
    }

    #[test]
    fn a_refusal_names_what_is_needed_after_the_credit_and_what_is_reused() {
        let f = figures(100 * GB, 60 * GB, 0);
        let why = judge(&f, &room(21 * GB), 0).unwrap_err();
        assert!(
            why.contains(&format!("needs {} more bytes", 40 * GB)),
            "{why}"
        );
        assert!(
            why.contains(&format!("short by {} bytes", 20 * GB)),
            "{why}"
        );
        assert!(
            why.contains(&format!("{} bytes", 60 * GB)) && why.contains("already on the console"),
            "{why}"
        );
    }

    #[test]
    fn a_fresh_refusal_does_not_claim_anything_is_reused() {
        let why = judge(&figures(50 * GB, 0, 0), &room(10 * GB), 0).unwrap_err();
        assert!(!why.contains("already on the console"), "{why}");
    }

    #[test]
    fn exactly_enough_is_enough() {
        let r = room(41 * GB); // 40 GiB allocatable
        assert!(judge(&figures(40 * GB, 0, 0), &r, 0).is_ok());
        assert!(judge(&figures(40 * GB + 1, 0, 0), &r, 0).is_err());
    }

    #[test]
    fn other_jobs_promises_come_off_the_room() {
        let r = room(101 * GB); // 100 GiB allocatable
        assert!(judge(&figures(60 * GB, 0, 0), &r, 0).is_ok());
        let why = judge(&figures(60 * GB, 0, 0), &r, 50 * GB).unwrap_err();
        assert!(why.contains("promised to other uploads"), "{why}");
    }

    #[test]
    fn a_promise_is_not_counted_twice_once_the_volume_has_paid_it() {
        let p = Promise {
            key: ("h".into(), "/data".into()),
            to_allocate: 100 * GB,
            room_then: 150 * GB,
            volume: "/data".into(),
            free_bytes: 0,
            reserve_bytes: 0,
        };
        assert_eq!(p.outstanding(150 * GB), 100 * GB, "nothing written yet");
        assert_eq!(p.outstanding(120 * GB), 70 * GB, "30 GB written");
        assert_eq!(p.outstanding(40 * GB), 0, "all of it written");
        assert_eq!(
            p.outstanding(200 * GB),
            100 * GB,
            "room grew: promise intact"
        );
    }

    fn probe_of(r: Room) -> RoomProbe {
        Arc::new(move |_, _| Some(r.clone()))
    }

    fn unique(tag: &str) -> Room {
        Room {
            volume: format!("/vol-{tag}"),
            ..room(101 * GB)
        }
    }

    #[test]
    fn two_jobs_cannot_both_be_admitted_into_room_for_one() {
        let r = unique("two-jobs");
        let (a, b) = ([0xa1; 16], [0xb1; 16]);
        let _ra = Reservation::new(a);
        let _rb = Reservation::new(b);
        let ga = gate(
            probe_of(r.clone()),
            "h-two".into(),
            "/vol-two-jobs/x".into(),
            a,
        );
        let gb = gate(
            probe_of(r.clone()),
            "h-two".into(),
            "/vol-two-jobs/y".into(),
            b,
        );
        let f = figures(60 * GB, 0, 0);
        assert!(ga(&f).is_ok());
        let why = gb(&f).expect_err("the second job would overfill the drive");
        assert!(why.contains("promised to other uploads"), "{why}");
        // A refused job holds nothing; the first job ending frees its promise.
        drop(_ra);
        assert!(gb(&f).is_ok());
    }

    #[test]
    fn a_jobs_own_reopen_replaces_its_promise_instead_of_counting_twice() {
        let r = unique("reopen");
        let a = [0xa2; 16];
        let _ra = Reservation::new(a);
        let g = gate(probe_of(r), "h-reopen".into(), "/vol-reopen/x".into(), a);
        let f = figures(60 * GB, 0, 0);
        assert!(g(&f).is_ok());
        assert!(
            g(&f).is_ok(),
            "a reconnect re-runs the check for the same job"
        );
    }

    #[test]
    fn jobs_on_other_volumes_or_consoles_do_not_compete() {
        let (a, b) = ([0xa3; 16], [0xb3; 16]);
        let _ra = Reservation::new(a);
        let _rb = Reservation::new(b);
        let ga = gate(probe_of(unique("v1")), "h-v".into(), "/vol-v1/x".into(), a);
        let gb = gate(probe_of(unique("v2")), "h-v".into(), "/vol-v2/x".into(), b);
        let f = figures(90 * GB, 0, 0);
        assert!(ga(&f).is_ok());
        assert!(gb(&f).is_ok());
    }

    #[test]
    fn an_unknown_room_is_never_a_refusal() {
        let a = [0xa4; 16];
        let none: RoomProbe = Arc::new(|_, _| None);
        let g = gate(none, "h".into(), "/x".into(), a);
        assert!(g(&figures(u64::MAX / 2, 0, 0)).is_ok());
    }
}
