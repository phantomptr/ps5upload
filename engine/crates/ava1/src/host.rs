//! A Rust job host: uploads into, and downloads from, one shared folder (engine↔engine,
//! and the tests). Policy and UI for sharing are deferred (plan scope).
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::conn::Frame;
use crate::gen::{self, JobOpen, JobOpenAck};
use crate::journal::{self, Journal, State};
use crate::manifest;
use crate::recv::{receive_job, resume_job, LocalSink, RecvOptions};
use crate::router::{JobHost, JobLink};
use crate::send::{serve_download, SendOptions};
use crate::source::LocalSource;
use crate::wire::FrameMessage;

/// Serves uploads into `root/<JobOpen.root relative>` and downloads from it. A
/// `JobOpen.root` that escapes `root` (absolute, `..`, any non-normal component) is
/// refused with `ERR_PATH`. This is the engine↔engine host minus its policy and UI
/// (deferred).
pub struct FolderHost {
    pub root: PathBuf,
    pub jobs_dir: PathBuf,
}

/// `rel` inside `root`, or None when it would escape (absolute, `..`, any non-normal
/// component). SPEC.md §11.2 — checked before anything touches the filesystem.
fn inside(root: &Path, rel: &str) -> Option<PathBuf> {
    if rel.is_empty() {
        return Some(root.to_path_buf()); // check_path's documented exception: the root itself
    }
    // `check_path` splits on '/', but Windows treats '\\' as a separator too, so
    // "a\\..\\..\\x" would pass the check and `root.join` would let it escape the share
    // on a Windows host. A JobOpen.root is a peer-chosen console/engine path — a
    // backslash is never a legitimate component of one — so refuse it outright.
    if rel.contains('\\') {
        return None;
    }
    if manifest::check_path(rel).is_err() {
        return None;
    }
    Some(root.join(rel))
}

impl JobHost for FolderHost {
    fn accept(&self, mut link: JobLink, first: Frame, peer: [u8; 32]) {
        let (root, jobs) = (self.root.clone(), self.jobs_dir.clone());
        tokio::spawn(async move {
            if first.ty == gen::Resume::TYPE {
                if let Ok(r) = first.decode::<gen::Resume>() {
                    answer_resume(&mut link, r, &root, &jobs, peer).await;
                }
                return; // a malformed Resume is dropped like any malformed frame
            }
            let Ok(open) = first.decode::<JobOpen>() else {
                return; // a malformed open: nothing this host serves
            };
            let Some(path) = inside(&root, &open.root) else {
                let _ = link
                    .control
                    .send(&JobOpenAck {
                        job_id: open.job_id,
                        status: gen::ERR_PATH,
                        credit: 0,
                        staged: 0,
                        workers: 0,
                        message: Some("outside the shared folder".into()),
                    })
                    .await;
                return;
            };
            match open.kind {
                gen::JOB_UPLOAD => {
                    let single = open.flags & gen::JF_SINGLE_FILE != 0;
                    let sink = Arc::new(LocalSink::new(path, single));
                    bind_peer(&jobs, &open.job_id, &peer);
                    let o = RecvOptions {
                        credit: 64 << 20,
                        flags: open.flags, // Q4: the journal's Open records the job's flags
                        jobs_dir: jobs,
                        ordered: open.flags & gen::JF_ORDERED != 0,
                        progress: Arc::default(),
                        cancel: Arc::default(),
                        progress_deadline: None,
                    };
                    let _ = receive_job(&mut link, open, sink, o).await;
                }
                gen::JOB_DOWNLOAD => {
                    // The peer-chosen root was checked above (ruling 14): walk/single emit
                    // only checked paths, so nothing peer-chosen reaches LocalSource
                    // unvalidated.
                    let (m, src) = if path.is_dir() {
                        let s = LocalSource::new(path);
                        (manifest::walk(&s, &|_: &str| false), s)
                    } else {
                        let s =
                            LocalSource::new(path.parent().unwrap_or(Path::new("/")).to_path_buf());
                        let name = path
                            .file_name()
                            .and_then(|n| n.to_str())
                            .unwrap_or("")
                            .to_string();
                        (manifest::single(&s, &name), s)
                    };
                    let Ok(m) = m else {
                        // The walk failed (a changed or unreadable source): refuse before
                        // any pages, so the opener's read_manifest ends instead of hanging.
                        let _ = link
                            .control
                            .send(&JobOpenAck {
                                job_id: open.job_id,
                                status: gen::ERR_IO,
                                credit: 0,
                                staged: 0,
                                workers: 0,
                                message: Some("the source cannot be read".into()),
                            })
                            .await;
                        return;
                    };
                    let mut o = SendOptions::upload("");
                    o.flags = open.flags;
                    let _ = serve_download(&mut link, open, Arc::new(m), Arc::new(src), o).await;
                }
                _ => {}
            }
        });
    }
}

/// The key of the peer that opened a job, kept beside its journal (SPEC.md §11.1: only that
/// key may resume it). Written once, when the job directory first exists.
const PEER_FILE: &str = "peer";

fn bind_peer(jobs: &Path, job: &[u8; 16], peer: &[u8; 32]) {
    let dir = journal::job_dir(jobs, job);
    if std::fs::create_dir_all(&dir).is_ok() && !dir.join(PEER_FILE).exists() {
        let _ = std::fs::write(dir.join(PEER_FILE), peer);
    }
}

/// SPEC.md §11.5: `Resume` for a parked upload of this peer whose stored manifest hashes to
/// `manifest_hash` continues the job (its `JobMap` goes out and data follows); anything else —
/// no journal, another peer's job, another manifest, a root outside the share — is
/// `JobMap{status = ERR_UNKNOWN_JOB}` (the two causes are not told apart), and the sender
/// falls back to `JobOpen`.
async fn answer_resume(
    link: &mut JobLink,
    r: gen::Resume,
    root: &Path,
    jobs: &Path,
    peer: [u8; 32],
) {
    let dir = journal::job_dir(jobs, &r.job_id);
    let known = (|| {
        if std::fs::read(dir.join(PEER_FILE)).ok()?.as_slice() != peer {
            return None;
        }
        let m = journal::read_manifest(&dir).ok()?;
        if m.hash() != r.manifest_hash {
            return None;
        }
        let (_, recs) = Journal::open(&dir).ok()?;
        let mut st = State::default();
        for rec in &recs {
            st.apply(rec);
        }
        let jo = st.open?;
        if jo.kind != gen::JOB_DOWNLOAD || jo.manifest_hash != r.manifest_hash {
            return None;
        }
        let dest = PathBuf::from(&jo.root);
        dest.starts_with(root).then_some((m, jo, dest))
    })();
    let Some((m, jo, dest)) = known else {
        let _ = link
            .control
            .send(&gen::JobMap {
                job_id: r.job_id,
                status: gen::ERR_UNKNOWN_JOB,
                last: 1,
                done: vec![],
                partial: vec![],
                message: None,
                held: None,
            })
            .await;
        return;
    };
    let single = jo.flags & gen::JF_SINGLE_FILE != 0;
    let o = RecvOptions {
        credit: 64 << 20,
        flags: jo.flags,
        jobs_dir: jobs.to_path_buf(),
        ordered: jo.flags & gen::JF_ORDERED != 0,
        progress: Arc::default(),
        cancel: Arc::default(),
        progress_deadline: None,
    };
    let _ = resume_job(link, m, Arc::new(LocalSink::new(dest, single)), o).await;
}
