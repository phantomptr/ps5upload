#![cfg(unix)]
//! payload/src/smp_meta.c: the sweep that fills /user/appmeta/<TITLE_ID>/ from the title's own
//! folder, run here against temporary folders standing in for /user/app and /user/appmeta.
use ps5upload_ctest::smp_meta;
use std::fs;
use std::os::unix::fs::{symlink, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// smp_meta.c keeps one set of roots and counters: one test at a time.
static LOCK: Mutex<()> = Mutex::new(());

struct Roots {
    base: PathBuf,
    app: PathBuf,
    meta: PathBuf,
}

impl Roots {
    fn new(tag: &str) -> Roots {
        let base = crate::test_tmp::temp_path(format!(
            "ps5u-smp-meta-{tag}-{}-{:?}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let app = base.join("app");
        let meta = base.join("appmeta");
        fs::create_dir_all(&app).unwrap();
        smp_meta::set_roots(&app, &meta);
        smp_meta::reset_stats();
        Roots { base, app, meta }
    }
    fn title(&self, id: &str) -> PathBuf {
        let d = self.app.join(id);
        fs::create_dir_all(d.join("sce_sys")).unwrap();
        d
    }
}

impl Drop for Roots {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.base);
    }
}

fn read(p: &Path) -> Vec<u8> {
    fs::read(p).unwrap()
}

/// No temporary files left beside the restored ones.
fn no_tmp_left(dir: &Path) {
    for e in fs::read_dir(dir).unwrap() {
        let n = e.unwrap().file_name().to_string_lossy().into_owned();
        assert!(!n.contains(".tmp-"), "leftover temporary {n}");
    }
}

#[test]
fn copies_icon_from_sce_sys_and_counts_it() {
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let r = Roots::new("sce");
    let t = r.title("PPSA01234");
    fs::write(t.join("sce_sys/icon0.png"), b"sce-icon").unwrap();
    fs::write(t.join("icon0.png"), b"root-icon").unwrap();
    fs::write(t.join("sce_sys/pic0.png"), b"pic0").unwrap();
    fs::write(t.join("sce_sys/param.json"), b"{\"a\":1}").unwrap();
    smp_meta::sweep();
    let m = r.meta.join("PPSA01234");
    assert_eq!(
        read(&m.join("icon0.png")),
        b"sce-icon",
        "sce_sys wins over the root"
    );
    assert_eq!(read(&m.join("pic0.png")), b"pic0");
    assert_eq!(read(&m.join("param.json")), b"{\"a\":1}");
    assert!(!m.join("pic1.png").exists());
    let mode = fs::metadata(m.join("icon0.png"))
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(mode & 0o022, 0, "not group/world writable: {mode:o}");
    no_tmp_left(&m);
    let s = smp_meta::stats();
    assert_eq!(s.games_scanned, 1);
    assert_eq!(s.icons_healed, 1);
    assert_eq!(s.pics_healed, 1);
    assert_eq!(s.json_healed, 1);
    assert_eq!(s.still_missing, 0);
    assert_eq!(s.last_missing, "");
    assert!(s.last_run_unix > 0);

    // A second sweep finds everything present: nothing more is copied.
    smp_meta::sweep();
    let s = smp_meta::stats();
    assert_eq!((s.icons_healed, s.pics_healed, s.json_healed), (1, 1, 1));
    assert_eq!(s.games_scanned, 1);
}

#[test]
fn falls_back_to_the_title_root() {
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let r = Roots::new("root");
    let t = r.title("CUSA00001");
    fs::write(t.join("icon0.png"), b"root-icon").unwrap();
    fs::write(t.join("snd0.at9"), b"snd").unwrap();
    // param.json only comes from sce_sys.
    fs::write(t.join("param.json"), b"{}").unwrap();
    smp_meta::sweep();
    let m = r.meta.join("CUSA00001");
    assert_eq!(read(&m.join("icon0.png")), b"root-icon");
    assert_eq!(read(&m.join("snd0.at9")), b"snd");
    assert!(!m.join("param.json").exists());
    let s = smp_meta::stats();
    assert_eq!((s.icons_healed, s.pics_healed, s.json_healed), (1, 1, 0));
    assert_eq!(s.still_missing, 0);
}

#[test]
fn symlink_and_empty_sources_are_ignored() {
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let r = Roots::new("link");
    let secret = r.base.join("secret.png");
    fs::write(&secret, b"outside").unwrap();
    let t = r.title("PPSA00002");
    symlink(&secret, t.join("sce_sys/icon0.png")).unwrap();
    fs::write(t.join("icon0.png"), b"").unwrap(); // empty: not a source
    symlink(&secret, t.join("sce_sys/param.json")).unwrap();
    smp_meta::sweep();
    let m = r.meta.join("PPSA00002");
    assert!(m.is_dir(), "the appmeta folder is still made");
    assert!(!m.join("icon0.png").exists());
    assert!(!m.join("param.json").exists());
    let s = smp_meta::stats();
    assert_eq!((s.icons_healed, s.pics_healed, s.json_healed), (0, 0, 0));
    assert_eq!(s.still_missing, 1);
    assert_eq!(s.last_missing, "PPSA00002");
}

#[test]
fn never_overwrites_an_existing_file_or_a_link() {
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let r = Roots::new("keep");
    let t = r.title("PPSA00003");
    for f in ["icon0.png", "pic0.png", "pic1.png", "param.json"] {
        fs::write(t.join("sce_sys").join(f), b"new").unwrap();
    }
    let m = r.meta.join("PPSA00003");
    fs::create_dir_all(&m).unwrap();
    fs::write(m.join("icon0.png"), b"old").unwrap();
    fs::write(m.join("pic0.png"), b"").unwrap(); // empty: replaced
    let target = r.base.join("target.png");
    fs::write(&target, b"target").unwrap();
    symlink(&target, m.join("pic1.png")).unwrap(); // a link at the destination: left alone
    fs::write(m.join("param.json"), b"{\"keep\":1}").unwrap();
    smp_meta::sweep();
    assert_eq!(read(&m.join("icon0.png")), b"old");
    assert_eq!(read(&m.join("pic0.png")), b"new");
    assert!(fs::symlink_metadata(m.join("pic1.png"))
        .unwrap()
        .file_type()
        .is_symlink());
    assert_eq!(read(&target), b"target", "the link's target is untouched");
    assert_eq!(read(&m.join("param.json")), b"{\"keep\":1}");
    no_tmp_left(&m);
    let s = smp_meta::stats();
    assert_eq!((s.icons_healed, s.pics_healed, s.json_healed), (0, 1, 0));
    assert_eq!(s.still_missing, 0, "icon0 was already present");
}

#[test]
fn invalid_names_and_non_folders_are_skipped() {
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let r = Roots::new("names");
    for bad in [
        "PPSA0123",      // 8: fine, the valid control below
        "PPSA012",       // 7: too short
        "PPSA012345678", // 13: too long
        "ppsa01234",     // lower case
        "PPSA_1234",     // underscore
    ] {
        let t = r.title(bad);
        fs::write(t.join("sce_sys/icon0.png"), b"i").unwrap();
    }
    fs::write(r.app.join("PPSA99999"), b"a file, not a folder").unwrap();
    symlink(r.app.join("PPSA0123"), r.app.join("PPSA88888")).unwrap();
    let twelve = r.title("ABCDEFGH1234");
    fs::write(twelve.join("sce_sys/icon0.png"), b"i").unwrap();
    smp_meta::sweep();
    let mut made: Vec<String> = fs::read_dir(&r.meta)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    made.sort();
    assert_eq!(made, ["ABCDEFGH1234", "PPSA0123"]);
    let s = smp_meta::stats();
    assert_eq!(s.games_scanned, 2);
    assert_eq!(s.icons_healed, 2);
}

#[test]
fn missing_counts_reset_per_sweep_but_last_missing_stays() {
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let r = Roots::new("miss");
    let t = r.title("PPSA00004");
    smp_meta::sweep();
    let s = smp_meta::stats();
    assert_eq!((s.games_scanned, s.still_missing), (1, 1));
    assert_eq!(s.last_missing, "PPSA00004");
    fs::write(t.join("sce_sys/icon0.png"), b"i").unwrap();
    smp_meta::sweep();
    let s = smp_meta::stats();
    assert_eq!(
        (s.games_scanned, s.still_missing, s.icons_healed),
        (1, 0, 1)
    );
    assert_eq!(
        s.last_missing, "PPSA00004",
        "kept until another title is missing"
    );
}

#[test]
fn an_unreadable_app_folder_ends_the_sweep_quietly() {
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let r = Roots::new("gone");
    let nowhere = r.base.join("does-not-exist");
    smp_meta::set_roots(&nowhere, &r.meta);
    smp_meta::sweep();
    let s = smp_meta::stats();
    assert_eq!((s.games_scanned, s.still_missing), (0, 0));
    assert!(!r.meta.exists(), "nothing written");
}

#[test]
fn oversized_sources_are_refused() {
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let r = Roots::new("big");
    let t = r.title("PPSA00005");
    let f = fs::File::create(t.join("sce_sys/pic0.png")).unwrap();
    f.set_len(32 * 1024 * 1024 + 1).unwrap(); // sparse
    fs::write(t.join("sce_sys/icon0.png"), vec![7u8; 200 * 1024]).unwrap(); // several buffers
    smp_meta::sweep();
    let m = r.meta.join("PPSA00005");
    assert!(!m.join("pic0.png").exists());
    assert_eq!(read(&m.join("icon0.png")), vec![7u8; 200 * 1024]);
    no_tmp_left(&m);
}

#[test]
fn poll_interval_is_clamped_and_run_now_returns_at_once() {
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    assert_eq!(smp_meta::set_poll_seconds(1), 5);
    assert_eq!(smp_meta::set_poll_seconds(10_000), 600);
    assert_eq!(smp_meta::set_poll_seconds(45), 45);
    assert_eq!(smp_meta::poll_seconds(), 45);
    assert_eq!(smp_meta::stats().poll_seconds, 45);
    assert_eq!(smp_meta::run_now(), 0);
    assert_eq!(smp_meta::set_poll_seconds(30), 30);
    assert_eq!(smp_meta::stats().running, 0, "not started by the hooks");
}

// Test scratch paths removed when the test process exits (engine/test-support/test_tmp.rs).
#[path = "../../../test-support/test_tmp.rs"]
mod test_tmp;
