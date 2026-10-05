#![cfg(unix)]
//! P3 Task 19: the old transfer and management servers, the transaction table, the spool and ports
//! 9113/9114 are gone from the payload. runtime.c is not compiled on the host, so these are checks
//! of the payload's own sources (and of the built ELF when one is newer than every source), plus
//! the one new piece of C that runs on the host: the first-start removal of the retired folders.
use std::ffi::CString;
use std::os::raw::{c_char, c_int};
use std::path::{Path, PathBuf};

// Links against ava1c (build.rs), which compiles payload/src/state_migrate.c.
use ava1_ctest as _;

extern "C" {
    fn payload_remove_retired_dirs(root: *const c_char) -> c_int;
}

fn payload() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../payload")
}

/// Every C/H source of the payload except the migration shim (and the vendored third party code).
fn sources() -> Vec<(PathBuf, String)> {
    let mut out = Vec::new();
    let mut stack = vec![payload()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).unwrap() {
            let p = e.unwrap().path();
            let name = p.file_name().unwrap().to_string_lossy().to_string();
            if p.is_dir() {
                if name == "third_party" || name == "build" || name == "fuzz" {
                    continue;
                }
                stack.push(p);
            } else if matches!(
                p.extension().and_then(|e| e.to_str()),
                Some("c") | Some("h") | Some("inc") | Some("def") | Some("py")
            ) && name != "legacy_takeover.c"
            {
                let t = std::fs::read_to_string(&p).unwrap_or_default();
                out.push((p, t));
            }
        }
    }
    out
}

fn tmp(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("ava1-cutover-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

#[test]
fn c_payload_binds_only_9120() {
    let files = sources();
    // The retired ports and the constants that carried them are gone from every source.
    for (p, t) in &files {
        for banned in [
            "PS5UPLOAD2_RUNTIME_PORT",
            "PS5UPLOAD2_MGMT_PORT",
            "PS5UPLOAD2_MAX_MGMT_THREADS",
            "runtime_server_loop",
            "runtime_mgmt_server_loop",
            "create_listener",
            "mgmt_listener_fd",
        ] {
            assert!(!t.contains(banned), "{} still names {banned}", p.display());
        }
    }
    // runtime.c opens no socket of its own: no listener, no accept loop.
    let rt = std::fs::read_to_string(payload().join("src/runtime.c")).unwrap();
    for banned in ["bind(", "listen(", "accept("] {
        assert!(!rt.contains(banned), "runtime.c still calls {banned}");
    }
    // The helper's default listener is the AVA1 server; the only other `bind(` sites are the
    // user-started FTP server (its own port) and the installer daemon (a separate process).
    let mut binders: Vec<String> = files
        .iter()
        .filter(|(_, t)| t.contains("bind(") || t.contains("listen("))
        .map(|(p, _)| {
            p.strip_prefix(payload())
                .unwrap()
                .to_string_lossy()
                .to_string()
        })
        .collect();
    binders.retain(|p| {
        !p.ends_with(".py") && !p.starts_with("tests/") && !p.starts_with("installer/")
    });
    binders.sort();
    assert_eq!(
        binders,
        vec!["ava1/ava1_server.c", "src/ftp_server.c"],
        "only the AVA1 server (9120) and the user-started FTP server open a listener"
    );
    // The AVA1 server binds AVA1_DEFAULT_PORT and the config carries no other helper port.
    let cfg = std::fs::read_to_string(payload().join("include/config.h")).unwrap();
    assert!(!cfg.contains("9113") && !cfg.contains("9114"));
    let gen = std::fs::read_to_string(payload().join("ava1/gen/ava1_gen.h")).unwrap();
    assert!(gen.contains("AVA1_DEFAULT_PORT 9120"));
}

#[test]
fn c_first_start_removes_the_ftx2_directories() {
    let root = tmp("firststart");
    for sub in ["tx", "spool/spool_ab/1", "ava/send", "runtime", "mounts"] {
        std::fs::create_dir_all(root.join(sub)).unwrap();
    }
    std::fs::write(root.join("tx/tx_1.json"), b"{}").unwrap();
    std::fs::write(root.join("tx/events.log"), b"x").unwrap();
    std::fs::write(root.join("spool/spool_ab/1/0"), vec![0u8; 4096]).unwrap();
    std::fs::write(root.join("ava/send/job.ob"), b"keep").unwrap();
    std::fs::write(root.join("ava/events.log"), b"keep").unwrap();
    std::fs::write(root.join("runtime/active_instance.txt"), b"keep").unwrap();
    // a symlink inside the retired folder must be unlinked, never followed
    let outside = tmp("firststart-outside");
    std::fs::write(outside.join("precious"), b"do not delete").unwrap();
    std::os::unix::fs::symlink(&outside, root.join("tx/link")).unwrap();

    let c = CString::new(root.to_str().unwrap()).unwrap();
    assert_eq!(unsafe { payload_remove_retired_dirs(c.as_ptr()) }, 0);
    assert!(!root.join("tx").exists(), "tx removed");
    assert!(!root.join("spool").exists(), "spool removed");
    assert_eq!(
        std::fs::read(root.join("ava/send/job.ob")).unwrap(),
        b"keep"
    );
    assert_eq!(std::fs::read(root.join("ava/events.log")).unwrap(), b"keep");
    assert!(root.join("runtime/active_instance.txt").exists());
    assert!(root.join("mounts").is_dir());
    assert_eq!(
        std::fs::read(outside.join("precious")).unwrap(),
        b"do not delete",
        "a link out of the folder is not followed"
    );
    // idempotent: the second start finds nothing and says so with 0
    assert_eq!(unsafe { payload_remove_retired_dirs(c.as_ptr()) }, 0);
    // a regular file named like a retired folder is removed too (it is ours either way)
    std::fs::write(root.join("tx"), b"file").unwrap();
    assert_eq!(unsafe { payload_remove_retired_dirs(c.as_ptr()) }, 0);
    assert!(!root.join("tx").exists());
    let _ = std::fs::remove_dir_all(&root);
    let _ = std::fs::remove_dir_all(&outside);
}

#[test]
fn c_first_start_never_creates_the_retired_directories() {
    let cfg = std::fs::read_to_string(payload().join("include/config.h")).unwrap();
    assert!(!cfg.contains("PS5UPLOAD2_TX_DIR") && !cfg.contains("PS5UPLOAD2_SPOOL_DIR"));
    let rt = std::fs::read_to_string(payload().join("src/runtime.c")).unwrap();
    assert!(!rt.contains("ps5upload/tx") && !rt.contains("ps5upload/spool"));
    // main.c runs the removal, and only after the takeover (an old helper may still be using them).
    let main = std::fs::read_to_string(payload().join("src/main.c")).unwrap();
    let take = main.find("runtime_try_takeover").expect("takeover in main");
    let rm = main
        .find("payload_remove_retired_dirs")
        .expect("main removes the retired folders");
    assert!(rm > take, "the removal runs after the takeover");
}

#[test]
fn c_no_frame_magic_ftx2_in_the_binary() {
    // Sources: no "FTX2" text, no magic constant, no 9113/9114 outside the migration shim.
    for (p, t) in sources() {
        let lower = t.to_lowercase();
        assert!(
            !lower.contains("ftx2"),
            "{} still says FTX2 (only legacy_takeover.c may)",
            p.display()
        );
        assert!(
            !lower.contains("0x32585446"),
            "{} has the magic",
            p.display()
        );
        for port in ["9113", "9114"] {
            let bytes = t.as_bytes();
            let mut i = 0;
            while let Some(k) = t[i..].find(port) {
                let a = i + k;
                let before = a.checked_sub(1).map(|j| bytes[j]);
                let after = bytes.get(a + 4).copied();
                let digit = |b: Option<u8>| b.map(|c| c.is_ascii_digit()).unwrap_or(false);
                assert!(
                    digit(before) || digit(after),
                    "{} names the retired port {port}",
                    p.display()
                );
                i = a + 4;
            }
        }
    }
    // The ELF, when one is built and newer than every source: only the shim's immediate may spell
    // the magic (the 4 bytes "FTX2" once), and no frame-name or port text is left.
    let elf = payload().join("ps5upload.elf");
    let Ok(meta) = std::fs::metadata(&elf) else {
        eprintln!("no ps5upload.elf: binary check skipped (build it with the SDK)");
        return;
    };
    let built = meta.modified().unwrap();
    let mut newest = std::time::UNIX_EPOCH;
    let mut stack = vec![
        payload().join("src"),
        payload().join("include"),
        payload().join("ava1"),
    ];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).unwrap() {
            let p = e.unwrap().path();
            if p.is_dir() {
                stack.push(p);
            } else if let Ok(m) = std::fs::metadata(&p).and_then(|m| m.modified()) {
                newest = newest.max(m);
            }
        }
    }
    if newest > built {
        eprintln!("ps5upload.elf is older than the sources: binary check skipped");
        return;
    }
    let bin = std::fs::read(&elf).unwrap();
    let count = |needle: &[u8]| bin.windows(needle.len()).filter(|w| *w == needle).count();
    assert!(count(b"FTX2") <= 1, "FTX2 appears {} times", count(b"FTX2"));
    for s in [
        &b"transfer_listener"[..],
        b"mgmt listener",
        b"BEGIN_TX",
        b"STREAM_SHARD",
        b"/data/ps5upload/tx",
        b"/data/ps5upload/spool",
    ] {
        assert_eq!(
            count(s),
            0,
            "{} still in the ELF",
            String::from_utf8_lossy(s)
        );
    }
}

#[test]
fn c_wire_constants_test_removed_not_skipped() {
    // payload_c_wire_constants_match went with the ftx2-proto crate's cross-check of runtime.c: it is
    // deleted, not #[ignore]d.
    let engine = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../crates");
    let mut stack = vec![engine];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).unwrap() {
            let p = e.unwrap().path();
            if p.is_dir() {
                if p.file_name().unwrap() == "target" {
                    continue;
                }
                stack.push(p);
            } else if p.extension().and_then(|e| e.to_str()) == Some("rs")
                && p.file_name().unwrap() != "payload_cutover.rs"
            {
                let t = std::fs::read_to_string(&p).unwrap_or_default();
                assert!(
                    !t.contains("payload_c_wire_constants_match"),
                    "{} still has the cross-check",
                    p.display()
                );
            }
        }
    }
}

/// Counts the rename(2) calls of every payload source file (comments skipped).
fn rename_sites() -> std::collections::BTreeMap<String, usize> {
    let mut out = std::collections::BTreeMap::new();
    for dir in ["src", "ava1", "installer"] {
        for e in std::fs::read_dir(payload().join(dir)).unwrap() {
            let p = e.unwrap().path();
            if p.extension().and_then(|e| e.to_str()) != Some("c") {
                continue;
            }
            let t = std::fs::read_to_string(&p).unwrap();
            let n = t
                .lines()
                .filter(|l| {
                    let l = l.trim_start();
                    !(l.starts_with("/*") || l.starts_with('*') || l.starts_with("//"))
                })
                .map(|l| {
                    let code = l.split("/*").next().unwrap_or(l);
                    let b = code.as_bytes();
                    let mut hits = 0;
                    let mut i = 0;
                    while let Some(k) = code[i..].find("rename(") {
                        let a = i + k;
                        let prev = a.checked_sub(1).map(|j| b[j]);
                        if !prev
                            .map(|c| c == b'_' || c.is_ascii_alphanumeric())
                            .unwrap_or(false)
                        {
                            hits += 1;
                        }
                        i = a + 7;
                    }
                    hits
                })
                .sum::<usize>();
            if n > 0 {
                out.insert(
                    format!("{dir}/{}", p.file_name().unwrap().to_string_lossy()),
                    n,
                );
            }
        }
    }
    out
}

#[test]
fn c_every_rename_site_is_audited() {
    // The audit table in cross_device.h lists every file with a rename(2) call and how many; a new site
    // must be added there with its reason (in the same directory, or guarded by the device check).
    let h = std::fs::read_to_string(payload().join("include/cross_device.h")).unwrap();
    let mut table = std::collections::BTreeMap::new();
    for l in h.lines() {
        let l = l.trim_start_matches([' ', '*']).trim();
        let mut it = l.split_whitespace();
        if let (Some(f), Some(n), Some(kind)) = (it.next(), it.next(), it.next()) {
            if (f.starts_with("src/") || f.starts_with("ava1/") || f.starts_with("installer/"))
                && f.ends_with(".c")
                && (kind == "dir" || kind == "guard" || kind == "1")
            {
                table.insert(f.to_string(), n.parse::<usize>().unwrap());
            }
        }
    }
    assert!(table.len() >= 10, "the audit table is in cross_device.h");
    assert_eq!(
        table,
        rename_sites(),
        "cross_device.h's RENAME AUDIT table is out of date"
    );
}
