//! FS_LIST_VOLUMES RPC — enumerate storage volumes visible on the PS5.
//!
//! The payload probes a fixed set of well-known mount points
//! (`/data`, `/user`, `/ext0..7`, `/usb0..7`) and returns an entry for each
//! that is currently mounted. This is intentionally a read-only probe: no
//! file contents are touched, only `lstat` + `statfs`.
//!
//! Typical use:
//!   - UI "pick a destination drive" dropdown
//!   - smoke/bench tests sanity-checking that `/data` is reachable
//!   - delta transfers choosing a target drive with sufficient free space

use anyhow::{bail, Context, Result};
use ftx2_proto::FrameType;
use serde::{Deserialize, Serialize};

use crate::connection::Connection;

/// Rough size of the PS5 content allocator's hidden pool — capacity that
/// `statfs(2)` on `/data`/`/user` advertises but the console will not actually
/// hand out. A FW 12.00 capture hit ENOSPC with ~86 GB still showing free.
///
/// **This is an estimate for explaining a failure, never for preventing one.**
/// It is deliberately NOT part of `safety_reserve_bytes()`. Subtracting it up
/// front looked reasonable with n=1 and was badly wrong in the field: the gap
/// is not a constant. Two later consoles measured 0 GB and ~58 GB against this
/// 80 GiB, so as a gate it refused transfers that fit easily — a 2.5 GiB pkg
/// onto a console with 86 GB free, and 70 GB onto one with 136 GB free.
///
/// Used only by the post-drop diagnosis, which runs after a transfer has
/// already failed and only decides how to word the error.
pub const INTERNAL_STORAGE_HIDDEN_RESERVE_ESTIMATE_BYTES: u64 = 80 * 1024 * 1024 * 1024;

/// Leave a small working margin on ordinary external filesystems for metadata,
/// journals, and activity elsewhere on the drive during a long upload. This is
/// a CAP, not a flat charge — see `external_reserve_for_total`.
pub const EXTERNAL_STORAGE_SAFETY_RESERVE_BYTES: u64 = 1024 * 1024 * 1024;

/// Working margin for an external/image filesystem, scaled to its size.
///
/// A flat 1 GiB is right for a real drive and absurd for a small mounted
/// disk image: a 64 MiB `.exfat` would reserve sixteen times its own
/// capacity, `allocatable_bytes()` would saturate to zero, and EVERY write
/// to it gets refused. Hardware-confirmed — writing 29 bytes into a mounted
/// 64 MiB image failed with "need 29 bytes, have 0 bytes".
///
/// 1/64th of the volume (~1.6%) keeps the same protection proportionally
/// while staying at the 1 GiB cap for anything above ~64 GiB. A volume that
/// reports no total at all reserves nothing — better to let the write attempt
/// proceed than to block on missing telemetry.
pub fn external_reserve_for_total(total_bytes: u64) -> u64 {
    EXTERNAL_STORAGE_SAFETY_RESERVE_BYTES.min(total_bytes / 64)
}

/// One entry in the payload's volume list.
///
/// Fields mirror `struct statfs` on PS5 FreeBSD: `fs_type` is the short
/// filesystem name (`ufs`, `bfs`, `nullfs`, `tmpfs`, …), `writable`
/// reflects the mount's `MNT_RDONLY` flag, `*_bytes` are derived from
/// block counts, and `mount_from` is the device / pseudo source
/// (`/dev/nvme1`, `/dev/ssd0.user`, `tmpfs`, ...).
///
/// `is_placeholder` is true for PS5 mount slots that have no real drive
/// attached (tmpfs or <256 MiB). UIs typically filter these out by
/// default but can show them to reflect hot-plug state.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Volume {
    pub path: String,
    #[serde(default)]
    pub mount_from: String,
    pub fs_type: String,
    pub total_bytes: u64,
    pub free_bytes: u64,
    pub writable: bool,
    #[serde(default)]
    pub is_placeholder: bool,
    /// For mounts under `/mnt/ps5upload/` this is the backing image file
    /// (`/data/homebrew/image.exfat`, etc.), recorded by the payload
    /// when the mount was created. Empty string for non-ours mounts or
    /// for mounts created before this tracking was added. UIs use this
    /// to surface "what file is mounted here" without needing to ask
    /// a different API.
    #[serde(default)]
    pub source_image: String,
    /// Capacity intentionally held back by the payload. New payloads publish
    /// this; older payloads omit it and the engine derives the same value from
    /// the mount identity via `safety_reserve_bytes()`.
    #[serde(default)]
    pub safety_reserve_bytes: u64,
    /// Bytes considered safe for a new allocation after the reserve above.
    /// This is advisory telemetry; callers should use `allocatable_bytes()` so
    /// responses from older payloads remain safe too.
    #[serde(default)]
    pub allocatable_bytes: u64,
}

impl Volume {
    /// Quick filter for "would a user call this a usable drive": present,
    /// not a placeholder, and has at least some free space. UIs building
    /// drive-picker dropdowns want this.
    pub fn is_usable(&self) -> bool {
        !self.is_placeholder && self.writable && self.free_bytes > 0
    }

    pub fn is_internal_user_storage(&self) -> bool {
        self.path == "/data" || self.path == "/user" || self.mount_from.contains("ssd0.user")
    }

    /// Capacity held back from the *blocking* capacity gate.
    ///
    /// This is a small filesystem working margin and nothing more. It must
    /// stay small: the gate it feeds refuses the transfer outright, so
    /// anything speculative here becomes a transfer the user cannot make at
    /// all. The console's hidden content-allocator pool is explicitly NOT
    /// modelled here — see `INTERNAL_STORAGE_HIDDEN_RESERVE_ESTIMATE_BYTES`
    /// for why guessing at it up front was a mistake.
    pub fn safety_reserve_bytes(&self) -> u64 {
        let local = external_reserve_for_total(self.total_bytes);
        if self.safety_reserve_bytes == 0 {
            return local;
        }
        if self.is_internal_user_storage() {
            // Payloads at 5.17.0 and earlier publish a flat 80 GiB here. A
            // user who updates the app but keeps an old payload on the
            // console would otherwise stay blocked by the very bug this
            // fixes, so an internal volume's published reserve is capped at
            // what we would compute ourselves.
            return self.safety_reserve_bytes.min(local);
        }
        self.safety_reserve_bytes
    }

    /// Free space adjusted by the *estimated* hidden allocator pool, for
    /// diagnosis only. Never use this to decide whether to start a transfer.
    pub fn diagnostic_allocatable_bytes(&self) -> u64 {
        if self.is_internal_user_storage() {
            self.free_bytes
                .saturating_sub(INTERNAL_STORAGE_HIDDEN_RESERVE_ESTIMATE_BYTES)
        } else {
            self.allocatable_bytes()
        }
    }

    pub fn allocatable_bytes(&self) -> u64 {
        // A legitimate new-payload response may publish zero when the drive is
        // inside its safety reserve, so only trust the wire value when nonzero.
        // Recomputing is equivalent in the zero case and handles old payloads.
        // Never trust a published value that is smaller than our own rule
        // would give: an old payload's flat 80 GiB internal reserve arrives
        // here pre-applied, and honouring it would reinstate the block that
        // `safety_reserve_bytes` above exists to lift.
        let local = self.free_bytes.saturating_sub(self.safety_reserve_bytes());
        if self.allocatable_bytes > 0 {
            self.allocatable_bytes.min(self.free_bytes).max(local)
        } else {
            local
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VolumeList {
    pub volumes: Vec<Volume>,
}

impl VolumeList {
    /// Find a volume by its mount-point path, for UIs that want to render
    /// a specific drive's free-space indicator without re-querying.
    pub fn find(&self, path: &str) -> Option<&Volume> {
        self.volumes.iter().find(|v| v.path == path)
    }

    /// Find the volume that hosts `dest_path` by longest-prefix match.
    /// E.g. `dest_path = "/mnt/ext0/games/big.pkg"` matched against a
    /// list containing `/`, `/data`, `/mnt/ext0` resolves to
    /// `/mnt/ext0` — the deepest mount that's a strict-segment prefix.
    ///
    /// `/data` matches `/data/foo` but NOT `/database/foo` — the
    /// prefix must end on a path separator (or equal the path).
    /// Returns `None` when no volume covers the path (caller should
    /// treat as "free space unknown" rather than refuse outright).
    pub fn find_for_path(&self, dest_path: &str) -> Option<&Volume> {
        let mut best: Option<&Volume> = None;
        for v in &self.volumes {
            if v.path.is_empty() {
                continue;
            }
            let sep = if v.path.ends_with('/') { "" } else { "/" };
            let prefix = format!("{}{}", v.path, sep);
            let is_match = dest_path == v.path || dest_path.starts_with(&prefix);
            if !is_match {
                continue;
            }
            match best {
                None => best = Some(v),
                Some(cur) if v.path.len() > cur.path.len() => best = Some(v),
                _ => {}
            }
        }
        best
    }
}

/// Connect to the payload, send FS_LIST_VOLUMES, await FS_LIST_VOLUMES_ACK,
/// return parsed list.
///
/// Returns an error if the payload replies with an unexpected frame type
/// (including `FrameType::Error`) or if the JSON body fails to parse.
pub fn list_volumes(addr: &str) -> Result<VolumeList> {
    let mut c = Connection::connect(addr)?;
    c.send_frame(FrameType::FsListVolumes, b"")?;
    let (hdr, resp) = c.recv_frame()?;
    let ft = hdr.frame_type().unwrap_or(FrameType::Error);
    if ft == FrameType::Error {
        bail!(
            "payload rejected FS_LIST_VOLUMES: {}",
            String::from_utf8_lossy(&resp)
        );
    }
    if ft != FrameType::FsListVolumesAck {
        bail!("expected FS_LIST_VOLUMES_ACK, got {:?}", ft);
    }
    let parsed: VolumeList =
        serde_json::from_slice(&resp).context("decode FS_LIST_VOLUMES_ACK body as JSON")?;
    Ok(parsed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_sample_response() {
        let body = br#"{"volumes":[
            {"path":"/data","mount_from":"/dev/ssd0.user","fs_type":"nullfs","total_bytes":800000000000,"free_bytes":500000000000,"writable":true,"is_placeholder":false},
            {"path":"/mnt/ext1","mount_from":"/dev/nvme1","fs_type":"bfs","total_bytes":1000000000000,"free_bytes":900000000000,"writable":true,"is_placeholder":false},
            {"path":"/mnt/ext0","mount_from":"tmpfs","fs_type":"tmpfs","total_bytes":2097152,"free_bytes":1736704,"writable":true,"is_placeholder":true}
        ]}"#;
        let parsed: VolumeList = serde_json::from_slice(body).unwrap();
        assert_eq!(parsed.volumes.len(), 3);
        let data = parsed.find("/data").expect("/data present");
        assert_eq!(data.mount_from, "/dev/ssd0.user");
        assert!(!data.is_placeholder);
        assert!(data.is_usable());
        let ext1 = parsed.find("/mnt/ext1").expect("/mnt/ext1 present");
        assert_eq!(ext1.mount_from, "/dev/nvme1");
        assert!(ext1.is_usable());
        let placeholder = parsed.find("/mnt/ext0").expect("placeholder present");
        assert!(placeholder.is_placeholder);
        assert!(!placeholder.is_usable(), "placeholder should not be usable");
    }

    #[test]
    fn find_for_path_longest_prefix() {
        let vlist = VolumeList {
            volumes: vec![
                Volume {
                    path: "/".to_string(),
                    mount_from: String::new(),
                    fs_type: "ufs".into(),
                    total_bytes: 0,
                    free_bytes: 0,
                    writable: true,
                    is_placeholder: false,
                    source_image: String::new(),
                    safety_reserve_bytes: 0,
                    allocatable_bytes: 0,
                },
                Volume {
                    path: "/data".to_string(),
                    mount_from: String::new(),
                    fs_type: "ufs".into(),
                    total_bytes: 0,
                    free_bytes: 1_000_000,
                    writable: true,
                    is_placeholder: false,
                    source_image: String::new(),
                    safety_reserve_bytes: 0,
                    allocatable_bytes: 0,
                },
                Volume {
                    path: "/mnt/ext0".to_string(),
                    mount_from: String::new(),
                    fs_type: "exfat".into(),
                    total_bytes: 0,
                    free_bytes: 50_000,
                    writable: true,
                    is_placeholder: false,
                    source_image: String::new(),
                    safety_reserve_bytes: 0,
                    allocatable_bytes: 0,
                },
            ],
        };
        // Deeper mount wins.
        assert_eq!(
            vlist.find_for_path("/mnt/ext0/games/big.pkg").unwrap().path,
            "/mnt/ext0"
        );
        // Exact path also matches.
        assert_eq!(vlist.find_for_path("/data").unwrap().path, "/data");
        // Strict-segment prefix: /data should NOT match /database/.
        // Should fall through to the deepest matching parent (/ root).
        assert_eq!(vlist.find_for_path("/database/x").unwrap().path, "/");
        // Path that no mount covers (no `/` root in the list, say) →
        // None.
        let no_root = VolumeList {
            volumes: vlist.volumes[1..].to_vec(),
        };
        assert!(no_root.find_for_path("/somewhere/else").is_none());
    }

    #[test]
    fn parse_empty_list() {
        let parsed: VolumeList = serde_json::from_slice(br#"{"volumes":[]}"#).unwrap();
        assert_eq!(parsed.volumes.len(), 0);
        assert!(parsed.find("/data").is_none());
    }

    #[test]
    fn small_mounted_images_stay_writable() {
        // Regression: a flat 1 GiB external reserve made every write to a
        // mounted disk image smaller than 1 GiB impossible. Reproduced on
        // hardware — writing 29 bytes into a mounted 64 MiB .exfat failed
        // with "need 29 bytes, have 0 bytes (29 bytes short)".
        let img: Volume = serde_json::from_str(
            r#"{"path":"/mnt/ps5upload/rwtest","mount_from":"/dev/lvd1","fs_type":"exfatfs","total_bytes":67108864,"free_bytes":66936832,"writable":true}"#,
        )
        .unwrap();
        assert!(
            img.allocatable_bytes() > 60 * 1024 * 1024,
            "a 64 MiB image must stay usable, got {} allocatable",
            img.allocatable_bytes()
        );
        assert_eq!(img.safety_reserve_bytes(), 67_108_864 / 64);

        // A real external drive still gets the full flat cap.
        let drive: Volume = serde_json::from_str(
            r#"{"path":"/mnt/usb0","mount_from":"/dev/da1p1","fs_type":"exfatfs","total_bytes":2000304078848,"free_bytes":971732418560,"writable":true}"#,
        )
        .unwrap();
        assert_eq!(
            drive.safety_reserve_bytes(),
            EXTERNAL_STORAGE_SAFETY_RESERVE_BYTES
        );

        // Internal storage gets the same small working margin as anything
        // else. It used to get a flat 80 GiB, which is what made the console
        // in `internal_gate_does_not_block_transfers_that_fit` unusable.
        let internal: Volume = serde_json::from_str(
            r#"{"path":"/data","mount_from":"/user/data","fs_type":"nullfs","total_bytes":673865203712,"free_bytes":609649819648,"writable":true}"#,
        )
        .unwrap();
        assert_eq!(
            internal.safety_reserve_bytes(),
            EXTERNAL_STORAGE_SAFETY_RESERVE_BYTES
        );

        // Unknown total reserves nothing rather than blocking everything.
        assert_eq!(external_reserve_for_total(0), 0);
    }

    #[test]
    fn capacity_reserve_is_conservative_and_backward_compatible() {
        let internal: Volume = serde_json::from_str(
            r#"{"path":"/data","mount_from":"/dev/ssd0.user","fs_type":"bfs","total_bytes":1000000000000,"free_bytes":150000000000,"writable":true}"#,
        )
        .unwrap();
        assert_eq!(
            internal.allocatable_bytes(),
            150_000_000_000 - EXTERNAL_STORAGE_SAFETY_RESERVE_BYTES
        );

        let external: Volume = serde_json::from_str(
            r#"{"path":"/mnt/ext1","mount_from":"/dev/nvme1","fs_type":"exfat","total_bytes":1000000000000,"free_bytes":900000000000,"writable":true,"safety_reserve_bytes":2147483648,"allocatable_bytes":897852516352}"#,
        )
        .unwrap();
        assert_eq!(external.safety_reserve_bytes(), 2 * 1024 * 1024 * 1024);
        assert_eq!(external.allocatable_bytes(), 897_852_516_352);
    }

    /// Both numbers here are verbatim from user bug reports against 5.17.0.
    /// Each was refused by the flat 80 GiB internal reserve while the console
    /// plainly had room, which is the whole reason that reserve is gone.
    #[test]
    fn internal_gate_does_not_block_transfers_that_fit() {
        // Report 1 (FW 11.00). Payload log: free=86285615104 reserve=85899345920
        // -> 386269184 allocatable, so a 2.5 GB pkg was rejected outright.
        let user: Volume = serde_json::from_str(
            r#"{"path":"/user","mount_from":"/dev/ssd0.user","fs_type":"ufs","total_bytes":904129740800,"free_bytes":86285615104,"writable":true}"#,
        )
        .unwrap();
        assert!(
            user.allocatable_bytes() >= 2_498_035_712,
            "a 2.5 GB pkg must fit in 86 GB of free space, got {} allocatable",
            user.allocatable_bytes()
        );

        // Report 2: BeginTx rejected a 70 GB game with 136 GB free.
        let data: Volume = serde_json::from_str(
            r#"{"path":"/data","mount_from":"/user/data","fs_type":"nullfs","total_bytes":904129740800,"free_bytes":136368816128,"writable":true}"#,
        )
        .unwrap();
        assert!(
            data.allocatable_bytes() >= 70_490_481_725,
            "a 70 GB game must fit in 136 GB of free space, got {} allocatable",
            data.allocatable_bytes()
        );
    }

    /// A console still running a 5.17.0-era payload publishes the old flat
    /// reserve on the wire. Updating only the app must still fix the user.
    #[test]
    fn stale_payload_reserve_does_not_reinstate_the_block() {
        let stale: Volume = serde_json::from_str(
            r#"{"path":"/user","mount_from":"/dev/ssd0.user","fs_type":"ufs","total_bytes":904129740800,"free_bytes":86285615104,"writable":true,"safety_reserve_bytes":85899345920,"allocatable_bytes":386269184}"#,
        )
        .unwrap();
        assert_eq!(
            stale.safety_reserve_bytes(),
            EXTERNAL_STORAGE_SAFETY_RESERVE_BYTES
        );
        assert!(
            stale.allocatable_bytes() >= 2_498_035_712,
            "got {} allocatable",
            stale.allocatable_bytes()
        );
    }

    /// The estimate survives, but only where it cannot block anything.
    #[test]
    fn hidden_reserve_estimate_is_diagnosis_only() {
        let full: Volume = serde_json::from_str(
            r#"{"path":"/data","mount_from":"/user/data","fs_type":"nullfs","total_bytes":947229556736,"free_bytes":85962588160,"writable":true}"#,
        )
        .unwrap();
        assert!(
            full.diagnostic_allocatable_bytes() < 1024 * 1024 * 1024,
            "the FW 12.00 console must still be diagnosable as full"
        );
        assert_eq!(
            full.allocatable_bytes(),
            85_962_588_160 - EXTERNAL_STORAGE_SAFETY_RESERVE_BYTES,
            "but it must not be blocked up front"
        );
    }

    #[test]
    fn user_chosen_mount_path_is_surfaced() {
        // Regression for 2.2.51: a .ffpkg mounted at a user-chosen path
        // (e.g. /data/homebrew/PPSA17599) used to be filtered out of the
        // FS_LIST_VOLUMES response because the payload's path-prefix
        // allowlist ran before the mount-tracker check. Resulting symptom:
        // mount succeeds, but Volumes tab and Library mount-badge never see
        // it, and games inside the image only show up if the /data
        // recursive walk reaches them under the entry cap.
        //
        // The deserializer-level test here documents the wire shape the
        // fixed payload now emits — a non-prefixed path with source_image
        // populated from the tracker file.
        let body = br#"{"volumes":[
            {"path":"/data/homebrew/PPSA17599","mount_from":"/dev/lvd0","fs_type":"ufs","total_bytes":50000000000,"free_bytes":0,"writable":true,"is_placeholder":false,"source_image":"/data/homebrew/PPSA17599.ffpkg"}
        ]}"#;
        let parsed: VolumeList = serde_json::from_slice(body).unwrap();
        let v = parsed
            .find("/data/homebrew/PPSA17599")
            .expect("user-chosen mount surfaced");
        assert_eq!(v.source_image, "/data/homebrew/PPSA17599.ffpkg");
        assert_eq!(v.fs_type, "ufs");
        assert!(v.writable);
    }

    #[test]
    fn parse_missing_new_fields_defaults_safely() {
        // Pre-upgrade mock server responses lack mount_from + is_placeholder;
        // serde-default should fill them so the upgrade is non-breaking.
        let body = br#"{"volumes":[{"path":"/data","fs_type":"ufs","total_bytes":100,"free_bytes":50,"writable":true}]}"#;
        let parsed: VolumeList = serde_json::from_slice(body).unwrap();
        assert_eq!(parsed.volumes.len(), 1);
        assert_eq!(parsed.volumes[0].mount_from, "");
        assert!(!parsed.volumes[0].is_placeholder);
    }
}
