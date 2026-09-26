//! What every [`RemoteFs`] must do, checked against whatever tree the server has under `root`.
//! It only reads, so it can run against a real share, FTP or SFTP server.

use super::{RemoteError, RemoteFs};

pub(crate) async fn check(fs: &dyn RemoteFs, root: &str) {
    let page = fs.list(root, None).await.expect("list the root");
    assert!(
        !page.entries.is_empty(),
        "the test tree under {root} is empty"
    );
    let mut saw_file = false;
    for e in &page.entries {
        let p = super::path::join(root, &e.name).unwrap();
        let st = fs.stat(&p).await.expect("stat a listed entry");
        assert_eq!(st.is_dir, e.is_dir, "{p}");
        if e.is_dir {
            continue;
        }
        saw_file = true;
        assert_eq!(st.size, e.size, "{p}");
        let f = fs.open(&p).await.expect("open a listed file");
        assert_eq!(f.size(), e.size, "{p}");
        let n = e.size.min(16);
        let head = f.read_at(0, n).await.expect("read the head");
        assert_eq!(head.len() as u64, n, "{p}");
        if e.size > 4 {
            let tail = f.read_at(e.size - 2, 16).await.expect("read past the end");
            assert_eq!(tail.len(), 2, "a read past the end comes back short, {p}");
            let mid = f.read_at(2, 2).await.unwrap();
            assert_eq!(mid, head[2..4], "positioned reads agree, {p}");
        }
    }
    assert!(
        saw_file,
        "the test tree under {root} has no file at its top level"
    );
    let walked = fs.walk(root, 10_000).await.expect("walk");
    assert!(walked
        .iter()
        .all(|(rel, e)| !e.is_dir && !rel.starts_with('/')));
    assert!(matches!(
        fs.stat(&format!(
            "{}/definitely-not-here-7f3a",
            root.trim_end_matches('/')
        ))
        .await,
        Err(RemoteError::NotFound(_))
    ));
}
