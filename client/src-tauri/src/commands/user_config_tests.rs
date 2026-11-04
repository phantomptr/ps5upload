use super::*;
use serde_json::json;
use std::path::PathBuf;

/// A folder of this test's own under the system temp folder, not created yet.
struct Scratch(PathBuf);

impl Scratch {
    fn new(name: &str) -> Self {
        let dir =
            std::env::temp_dir().join(format!("ps5upload-config-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        Self(dir)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn a_missing_or_empty_file_is_no_settings() {
    let dir = Scratch::new("missing");
    let path = dir.0.join("settings.json");
    assert_eq!(load_from(&path).unwrap(), JsonValue::Null);
    std::fs::create_dir_all(&dir.0).unwrap();
    std::fs::write(&path, b"").unwrap();
    assert_eq!(load_from(&path).unwrap(), JsonValue::Null);
}

/// People edit this file by hand: a typo must be reported, not read as "no settings", which the
/// renderer would then save its defaults over.
#[test]
fn a_damaged_file_is_an_error_and_stays_on_disk() {
    let dir = Scratch::new("damaged");
    std::fs::create_dir_all(&dir.0).unwrap();
    let path = dir.0.join("settings.json");
    std::fs::write(&path, b"{\"theme\": ").unwrap();
    assert!(load_from(&path).unwrap_err().contains("parse"));
    assert_eq!(std::fs::read(&path).unwrap(), b"{\"theme\": ");
}

#[test]
fn a_save_makes_the_folder_reads_back_and_leaves_no_temporary() {
    let dir = Scratch::new("save");
    let path = dir.0.join("deep").join("settings.json");
    save_to(&path, &json!({"theme": "dark"})).unwrap();
    save_to(&path, &json!({"theme": "light", "n": 2})).unwrap();
    assert_eq!(load_from(&path).unwrap(), json!({"theme": "light", "n": 2}));
    let names: Vec<_> = std::fs::read_dir(path.parent().unwrap())
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    assert_eq!(names, ["settings.json"]);
}

/// Kept readable: the point of this file is that people can open it.
#[test]
fn the_file_is_indented() {
    let dir = Scratch::new("pretty");
    let path = dir.0.join("settings.json");
    save_to(&path, &json!({"theme": "dark"})).unwrap();
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "{\n  \"theme\": \"dark\"\n}"
    );
}

/// The renderer's two savers run at once (see `TMP_SEQ`): each needs its own temporary, and
/// whichever lands last leaves a whole file. Unix only: Windows refuses to replace a file another
/// thread is replacing, which the renderer's next save puts right.
#[cfg(unix)]
#[test]
fn saves_at_the_same_time_all_succeed_and_leave_a_whole_file() {
    let dir = Scratch::new("concurrent");
    let path = dir.0.join("settings.json");
    let threads: Vec<_> = (0..8)
        .map(|i| {
            let path = path.clone();
            std::thread::spawn(move || {
                save_to(&path, &json!({ "writer": i, "pad": "x".repeat(4096) }))
            })
        })
        .collect();
    for t in threads {
        t.join().unwrap().unwrap();
    }
    assert!(load_from(&path).unwrap()["writer"].is_i64());
    let names: Vec<_> = std::fs::read_dir(&dir.0)
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    assert_eq!(names, ["settings.json"]);
}

/// A save that cannot land leaves no `settings.json.tmp.N` behind to pile up.
#[test]
fn a_failed_save_cleans_up_its_temporary() {
    let dir = Scratch::new("failed");
    // A folder with a file in it where the settings file goes: the rename is refused.
    let path = dir.0.join("settings.json");
    std::fs::create_dir_all(&path).unwrap();
    std::fs::write(path.join("inside"), b"kept").unwrap();
    assert!(save_to(&path, &json!({})).is_err());
    assert_eq!(std::fs::read(path.join("inside")).unwrap(), b"kept");
    let names: Vec<_> = std::fs::read_dir(&dir.0)
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    assert_eq!(names, ["settings.json"]);
}

#[test]
fn the_engine_url_is_read_only_when_it_says_something() {
    let url = |v: JsonValue| engine_url_in(&serde_json::to_vec(&v).unwrap());
    assert_eq!(
        url(json!({"engine_url": " http://nas:19113 "})).as_deref(),
        Some("http://nas:19113")
    );
    assert_eq!(url(json!({"engine_url": "   "})), None);
    assert_eq!(url(json!({"engine_url": 5})), None);
    assert_eq!(url(json!({"theme": "dark"})), None);
    assert_eq!(engine_url_in(b"{not json"), None);
}
