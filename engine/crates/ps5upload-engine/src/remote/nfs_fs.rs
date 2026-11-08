//! NFS exports: an NFSv3 client (RFC 1813) over ONC RPC on TCP (RFC 5531), for NAS boxes and Linux
//! servers. The portmapper (RFC 1833) says where the MOUNT service is; a server without one (go-nfs,
//! rclone) is asked on the NFS port. Calls carry AUTH_SYS with the connection's `uid:gid`, the
//! connection's user field, from a source port below 1024 when this system allows it: exports marked
//! "secure", the Linux default, refuse any other.
//!
//! The connection's share is the export. Without one, the server's exports are folders:
//! `/volume1/games` is reached as volume1 > games. The approach and the export tree follow
//! fthdgn-gh/Alembic's client (GPL-3.0-or-later), which is written from the same RFCs.

use std::collections::{BTreeSet, HashMap};
use std::sync::Arc;
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpSocket, TcpStream};
use tokio::sync::Mutex;

use super::store::Connection;
use super::{Entry, Page, RemoteError, RemoteFile, RemoteFs};

const PORTMAP: u32 = 100_000;
const MOUNT: u32 = 100_005;
const NFS: u32 = 100_003;
const GETPORT: u32 = 3;
const MNT: u32 = 1;
const EXPORT: u32 = 5;
const GETATTR: u32 = 1;
const LOOKUP: u32 = 3;
const READ: u32 = 6;
const WRITE: u32 = 7;
const CREATE: u32 = 8;
const MKDIR: u32 = 9;
const REMOVE: u32 = 12;
const RMDIR: u32 = 13;
const RENAME: u32 = 14;
const READDIR: u32 = 16;
const READDIRPLUS: u32 = 17;
const FSSTAT: u32 = 18;
const FSINFO: u32 = 19;
const COMMIT: u32 = 21;
const TYPE_DIR: u32 = 2;
const NOT_SUPPORTED: u32 = 10004;
const STALE: u32 = 70;
const STEP: Duration = Duration::from_secs(30);
const MAX_RECORD: usize = 64 << 20;
/// The most entries a folder listing takes: a server that never ends one isn't waited for.
const MAX_LISTED: usize = 2_000_000;

fn status_text(s: u32) -> String {
    match s {
        1 => "not allowed (the export maps this uid:gid to a user without rights)",
        2 => "no such file or folder",
        5 => "input/output error on the server",
        13 => "access denied",
        17 => "it already exists",
        18 => "can't move across file systems",
        20 => "not a folder",
        21 => "it is a folder",
        27 => "the file is too large",
        28 => "the disk is full",
        30 => "the export is read-only",
        63 => "the name is too long",
        66 => "the folder is not empty",
        69 => "over the quota",
        70 => "stale file handle (moved or deleted on the server)",
        10004 => "not supported by the server",
        10006 => "server fault",
        10008 => "the server is busy, try again",
        other => return format!("error {other}"),
    }
    .to_string()
}

fn status_error(what: &str, s: u32) -> RemoteError {
    let text = format!("{what}: {}", status_text(s));
    match s {
        2 => RemoteError::NotFound(text),
        1 | 13 => RemoteError::Auth(text),
        17 => RemoteError::Exists(text),
        _ => RemoteError::Io(text),
    }
}

/// XDR (RFC 4506): big-endian, everything padded to 4 bytes.
#[derive(Default)]
struct Xdr(Vec<u8>);

impl Xdr {
    fn u32(mut self, v: u32) -> Self {
        self.0.extend_from_slice(&v.to_be_bytes());
        self
    }
    fn u64(mut self, v: u64) -> Self {
        self.0.extend_from_slice(&v.to_be_bytes());
        self
    }
    fn fixed(mut self, b: &[u8]) -> Self {
        self.0.extend_from_slice(b);
        while !self.0.len().is_multiple_of(4) {
            self.0.push(0);
        }
        self
    }
    fn opaque(self, b: &[u8]) -> Self {
        self.u32(b.len() as u32).fixed(b)
    }
    fn dir_op(self, dir: &[u8], name: &str) -> Self {
        self.opaque(dir).opaque(name.as_bytes())
    }
}

/// A reply being read; running off its end sets `bad` and reads as zeros.
struct In<'a> {
    b: &'a [u8],
    at: usize,
    bad: bool,
}

impl<'a> In<'a> {
    fn new(b: &'a [u8]) -> Self {
        Self {
            b,
            at: 0,
            bad: false,
        }
    }
    fn take(&mut self, n: usize) -> &'a [u8] {
        match self.at.checked_add(n).filter(|end| *end <= self.b.len()) {
            Some(end) => {
                let out = &self.b[self.at..end];
                self.at = end;
                out
            }
            None => {
                self.bad = true;
                self.at = self.b.len();
                &[]
            }
        }
    }
    fn u32(&mut self) -> u32 {
        let v = self.take(4);
        if v.len() == 4 {
            u32::from_be_bytes([v[0], v[1], v[2], v[3]])
        } else {
            0
        }
    }
    fn u64(&mut self) -> u64 {
        (u64::from(self.u32()) << 32) | u64::from(self.u32())
    }
    fn skip(&mut self, n: usize) {
        self.take(n.saturating_add(3) & !3);
    }
    fn opaque(&mut self, max: usize) -> Vec<u8> {
        let n = self.u32() as usize;
        if n > max {
            self.bad = true;
            return Vec::new();
        }
        let out = self.take(n).to_vec();
        self.skip_padding(n);
        out
    }
    fn skip_padding(&mut self, n: usize) {
        self.take((4 - n % 4) % 4);
    }
    fn text(&mut self) -> String {
        String::from_utf8_lossy(&self.opaque(4096)).into_owned()
    }
}

#[derive(Clone, Copy, Default, Debug, PartialEq)]
struct Attr {
    kind: u32,
    size: u64,
    mtime: i64,
}

fn fattr3(i: &mut In) -> Attr {
    let kind = i.u32();
    i.skip(16); // mode, nlink, uid, gid
    let size = i.u64();
    i.skip(32); // used, rdev, fsid, fileid
    i.skip(8); // atime
    let mtime = i64::from(i.u32());
    i.skip(4);
    i.skip(8); // ctime
    Attr { kind, size, mtime }
}

fn post_op_attr(i: &mut In) -> Option<Attr> {
    (i.u32() != 0).then(|| fattr3(i))
}

fn wcc_data(i: &mut In) {
    if i.u32() != 0 {
        i.skip(24);
    }
    post_op_attr(i);
}

/// One TCP connection to an RPC program, with record marking and one call at a time.
struct Rpc {
    stream: TcpStream,
    program: u32,
    version: u32,
    uid: u32,
    gid: u32,
    next_xid: u32,
}

async fn dial(host: &str, port: u16) -> Result<TcpStream, RemoteError> {
    let addrs: Vec<_> = tokio::net::lookup_host((host, port))
        .await
        .map_err(|e| RemoteError::Unreachable(format!("{host}: {e}")))?
        .collect();
    let addr = addrs
        .first()
        .copied()
        .ok_or_else(|| RemoteError::Unreachable(format!("{host}: no address")))?;
    let socket = if addr.is_ipv4() {
        TcpSocket::new_v4()
    } else {
        TcpSocket::new_v6()
    }
    .map_err(|e| RemoteError::Io(e.to_string()))?;
    // Exports marked "secure" want a port below 1024, which only an administrator can bind.
    for local in (665..1024u16).rev().take(32) {
        let any: std::net::IpAddr = if addr.is_ipv4() {
            std::net::Ipv4Addr::UNSPECIFIED.into()
        } else {
            std::net::Ipv6Addr::UNSPECIFIED.into()
        };
        match socket.bind((any, local).into()) {
            Ok(()) => break,
            Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => break,
            Err(_) => {}
        }
    }
    tokio::time::timeout(STEP, socket.connect(addr))
        .await
        .map_err(|_| RemoteError::Unreachable(format!("{host}:{port}: no answer after 30 s")))?
        .map_err(|e| RemoteError::Unreachable(format!("{host}:{port}: {e}")))
}

impl Rpc {
    async fn open(
        host: &str,
        port: u16,
        program: u32,
        version: u32,
        uid: u32,
        gid: u32,
    ) -> Result<Self, RemoteError> {
        Ok(Self {
            stream: dial(host, port).await?,
            program,
            version,
            uid,
            gid,
            next_xid: 0x5053_0000,
        })
    }

    async fn call(&mut self, proc: u32, args: &Xdr) -> Result<Vec<u8>, RemoteError> {
        let xid = self.next_xid;
        self.next_xid = self.next_xid.wrapping_add(1);
        let mut m = Xdr::default()
            .u32(xid)
            .u32(0) // a call
            .u32(2)
            .u32(self.program)
            .u32(self.version)
            .u32(proc);
        m = if self.program == PORTMAP {
            m.u32(0).u32(0)
        } else {
            let cred = Xdr::default()
                .u32(0)
                .opaque(b"ps5upload")
                .u32(self.uid)
                .u32(self.gid)
                .u32(1)
                .u32(self.gid);
            m.u32(1).opaque(&cred.0)
        };
        m = m.u32(0).u32(0); // verifier
        m.0.extend_from_slice(&args.0);
        let mark = 0x8000_0000u32 | m.0.len() as u32;
        let io = |e: std::io::Error| RemoteError::Io(format!("NFS: {e}"));
        let exchange = async {
            self.stream
                .write_all(&mark.to_be_bytes())
                .await
                .map_err(io)?;
            self.stream.write_all(&m.0).await.map_err(io)?;
            loop {
                let record = self.read_record().await?;
                if let Some(results) = parse_reply(&record, xid)? {
                    return Ok(results);
                }
            }
        };
        tokio::time::timeout(STEP, exchange)
            .await
            .map_err(|_| RemoteError::Io("NFS: no answer after 30 s".to_string()))?
    }

    async fn read_record(&mut self) -> Result<Vec<u8>, RemoteError> {
        let mut record = Vec::new();
        loop {
            let mut head = [0u8; 4];
            let io = |e: std::io::Error| RemoteError::Io(format!("NFS: {e}"));
            self.stream.read_exact(&mut head).await.map_err(io)?;
            let mark = u32::from_be_bytes(head);
            let n = (mark & 0x7FFF_FFFF) as usize;
            if record.len() + n > MAX_RECORD {
                return Err(RemoteError::Io("NFS: reply too large".to_string()));
            }
            let at = record.len();
            record.resize(at + n, 0);
            self.stream
                .read_exact(&mut record[at..])
                .await
                .map_err(io)?;
            if mark & 0x8000_0000 != 0 {
                return Ok(record);
            }
        }
    }
}

/// The results of a reply to `xid`; `None` for a reply to anything else.
fn parse_reply(record: &[u8], xid: u32) -> Result<Option<Vec<u8>>, RemoteError> {
    let mut i = In::new(record);
    let id = i.u32();
    if i.u32() != 1 || id != xid {
        return Ok(None);
    }
    if i.u32() != 0 {
        return Err(if i.u32() == 1 {
            RemoteError::Auth(
                "NFS: the server refused the login: check the user and group ID, and that the \
                 export lets this computer in"
                    .to_string(),
            )
        } else {
            RemoteError::Io("NFS: the server doesn't speak this RPC version".to_string())
        });
    }
    i.u32(); // verifier flavour
    i.opaque(400);
    let accept = i.u32();
    if i.bad {
        return Err(RemoteError::Io("NFS: malformed reply".to_string()));
    }
    match accept {
        0 => Ok(Some(record[i.at..].to_vec())),
        1 => Err(RemoteError::Unreachable(
            "NFS: that program isn't available on this port".to_string(),
        )),
        2 => Err(RemoteError::Io(
            "NFS: the server doesn't speak NFS version 3".to_string(),
        )),
        3 => Err(RemoteError::Unsupported(
            "NFS: the server doesn't offer that call".to_string(),
        )),
        _ => Err(RemoteError::Io(
            "NFS: the server could not decode a request".to_string(),
        )),
    }
}

fn split(path: &str) -> Vec<&str> {
    path.split('/')
        .filter(|p| !p.is_empty() && *p != ".")
        .collect()
}

fn joined(parts: &[&str]) -> String {
    format!("/{}", parts.join("/"))
}

/// The names one level below `dir` in a set of export paths.
fn export_children(exports: &[String], dir: &str) -> Vec<String> {
    let dir = split(dir);
    let mut names = BTreeSet::new();
    for e in exports {
        let parts = split(e);
        if parts.len() > dir.len() && parts[..dir.len()] == dir[..] {
            names.insert(parts[dir.len()].to_string());
        }
    }
    names.into_iter().collect()
}

/// The export a path is in: the longest that it starts with.
fn export_of(exports: &[String], path: &str) -> Option<String> {
    let p = split(path);
    exports
        .iter()
        .filter(|e| {
            let parts = split(e);
            parts.len() <= p.len() && parts[..] == p[..parts.len()]
        })
        .max_by_key(|e| split(e).len())
        .cloned()
}

/// `uid:gid` from the connection's user field: `1000:1000`, or `1000` for both; empty is root.
fn parse_ids(text: &str) -> Result<(u32, u32), RemoteError> {
    let t = text.trim();
    if t.is_empty() {
        return Ok((0, 0));
    }
    let (u, g) = t.split_once(':').unwrap_or((t, t));
    match (u.trim().parse(), g.trim().parse()) {
        (Ok(uid), Ok(gid)) => Ok((uid, gid)),
        _ => Err(RemoteError::Auth(format!(
            "NFS has no passwords: the user field is a user and group ID, such as 1000:1000, not {t:?}"
        ))),
    }
}

struct State {
    nfs: Rpc,
    mount_port: Option<u16>,
    exports: Option<Vec<String>>,
    roots: HashMap<String, Vec<u8>>,
    /// Handles of paths already looked up, by export and path.
    handles: HashMap<String, Vec<u8>>,
}

struct Inner {
    host: String,
    port: u16,
    uid: u32,
    gid: u32,
    /// The export of the connection, `/volume1/games`; empty for all of them.
    root: String,
    state: Mutex<State>,
    read_size: Mutex<u32>,
    write_size: Mutex<u32>,
}

impl Inner {
    async fn nfs_call(&self, proc: u32, args: Xdr) -> Result<Vec<u8>, RemoteError> {
        let mut s = self.state.lock().await;
        match s.nfs.call(proc, &args).await {
            Err(RemoteError::Io(m)) => {
                s.nfs = Rpc::open(&self.host, self.port, NFS, 3, self.uid, self.gid).await?;
                // A server that dropped an idle connection: once more on a new one, for a call
                // that gives the same answer twice. One that changes something may have been done
                // already, with only its reply lost, so saying it failed is all that can be said.
                if matches!(
                    proc,
                    GETATTR
                        | LOOKUP
                        | READ
                        | WRITE
                        | READDIR
                        | READDIRPLUS
                        | FSSTAT
                        | FSINFO
                        | COMMIT
                ) {
                    s.nfs.call(proc, &args).await
                } else {
                    Err(RemoteError::Io(m))
                }
            }
            other => other,
        }
    }

    async fn mount_port(&self) -> u16 {
        if let Some(p) = self.state.lock().await.mount_port {
            return p;
        }
        let mut port = self.port;
        let asked = async {
            let mut pm = Rpc::open(&self.host, 111, PORTMAP, 2, 0, 0).await?;
            let args = Xdr::default().u32(MOUNT).u32(3).u32(6).u32(0);
            pm.call(GETPORT, &args).await
        };
        if let Ok(Ok(reply)) = tokio::time::timeout(Duration::from_secs(3), asked).await {
            let mut i = In::new(&reply);
            let p = i.u32();
            if !i.bad && p > 0 && p < 65536 {
                port = p as u16;
            }
        }
        self.state.lock().await.mount_port = Some(port);
        port
    }

    async fn mount(&self, export: &str) -> Result<Vec<u8>, RemoteError> {
        if let Some(h) = self.state.lock().await.roots.get(export) {
            return Ok(h.clone());
        }
        let port = self.mount_port().await;
        let mut m = Rpc::open(&self.host, port, MOUNT, 3, self.uid, self.gid).await?;
        let reply = m
            .call(MNT, &Xdr::default().opaque(export.as_bytes()))
            .await?;
        let mut i = In::new(&reply);
        match i.u32() {
            0 => {}
            1 | 13 => {
                return Err(RemoteError::Auth(format!(
                    "the server doesn't let this computer mount {export}: check the export's \
                     allowed addresses. Exports marked \"secure\" need a port below 1024, so \
                     mark it \"insecure\" or run the app as an administrator"
                )))
            }
            2 => {
                return Err(RemoteError::NotFound(format!(
                    "the server has no export {export}"
                )))
            }
            s => return Err(status_error(&format!("mounting {export}"), s)),
        }
        let handle = i.opaque(64);
        if i.bad || handle.is_empty() {
            return Err(RemoteError::Io("NFS: malformed mount reply".to_string()));
        }
        self.state
            .lock()
            .await
            .roots
            .insert(export.to_string(), handle.clone());
        Ok(handle)
    }

    async fn exports(&self) -> Result<Vec<String>, RemoteError> {
        if let Some(e) = &self.state.lock().await.exports {
            return Ok(e.clone());
        }
        let port = self.mount_port().await;
        let mut m = Rpc::open(&self.host, port, MOUNT, 3, self.uid, self.gid).await?;
        let mut list = Vec::new();
        if let Ok(reply) = m.call(EXPORT, &Xdr::default()).await {
            let mut i = In::new(&reply);
            while !i.bad && i.u32() != 0 {
                let dir = i.text();
                while !i.bad && i.u32() != 0 {
                    i.text();
                }
                if !i.bad && dir.starts_with('/') {
                    list.push(dir);
                }
            }
        }
        // A server that lists nothing (go-nfs, rclone) serves one tree at "/".
        if list.is_empty() {
            list.push("/".to_string());
        }
        self.state.lock().await.exports = Some(list.clone());
        Ok(list)
    }

    /// The export `path` is in and the rest of the path; `None` for a place above the exports.
    async fn place(&self, path: &str) -> Result<Option<(String, String)>, RemoteError> {
        if !self.root.is_empty() {
            return Ok(Some((self.root.clone(), joined(&split(path)))));
        }
        let all = self.exports().await?;
        Ok(export_of(&all, path).map(|e| {
            let skip = split(&e).len();
            let p = split(path);
            (e, joined(&p[skip..]))
        }))
    }

    async fn lookup(&self, dir: &[u8], name: &str) -> Result<(Vec<u8>, Option<Attr>), RemoteError> {
        let reply = self
            .nfs_call(LOOKUP, Xdr::default().dir_op(dir, name))
            .await?;
        let mut i = In::new(&reply);
        let status = i.u32();
        if status != 0 {
            return Err(status_error(name, status));
        }
        let handle = i.opaque(64);
        let attr = post_op_attr(&mut i);
        if i.bad {
            return Err(RemoteError::Io("NFS: malformed lookup reply".to_string()));
        }
        Ok((handle, attr))
    }

    /// The handle of a place inside an export.
    async fn resolve(&self, export: &str, rel: &str) -> Result<Vec<u8>, RemoteError> {
        let key = format!("{export}\0{rel}");
        if let Some(h) = self.state.lock().await.handles.get(&key) {
            return Ok(h.clone());
        }
        let mut handle = self.mount(export).await?;
        for part in split(rel) {
            handle = self.lookup(&handle, part).await?.0;
        }
        let mut s = self.state.lock().await;
        // Enough for a walk through a big library; a stale one clears them all.
        if s.handles.len() > 4096 {
            s.handles.clear();
        }
        s.handles.insert(key, handle.clone());
        Ok(handle)
    }

    async fn forget_handles(&self) {
        self.state.lock().await.handles.clear();
    }

    /// The handle of `path`, once more from the top if the first came back stale.
    async fn handle_of(&self, path: &str) -> Result<Vec<u8>, RemoteError> {
        let Some((export, rel)) = self.place(path).await? else {
            return Err(RemoteError::NotFound(format!(
                "{path} is above the exports"
            )));
        };
        self.resolve(&export, &rel).await
    }

    /// A path's folder and name.
    async fn parent_of(&self, path: &str) -> Result<(Vec<u8>, String), RemoteError> {
        let parts = split(path);
        let Some((name, dir)) = parts.split_last() else {
            return Err(RemoteError::Io(
                "that is the root of the server".to_string(),
            ));
        };
        Ok((self.handle_of(&joined(dir)).await?, (*name).to_string()))
    }

    async fn read_limits(&self, root: &[u8]) {
        let Ok(reply) = self.nfs_call(FSINFO, Xdr::default().opaque(root)).await else {
            return;
        };
        let mut i = In::new(&reply);
        if i.u32() != 0 {
            return;
        }
        post_op_attr(&mut i);
        let (rtmax, rtpref) = (i.u32(), i.u32());
        i.u32();
        let (wtmax, wtpref) = (i.u32(), i.u32());
        if i.bad {
            return;
        }
        // As much as the server takes per request, up to 1 MiB.
        let pick = |max: u32, pref: u32| (if max != 0 { max } else { pref }).clamp(4096, 1 << 20);
        *self.read_size.lock().await = pick(rtmax, rtpref);
        *self.write_size.lock().await = pick(wtmax, wtpref);
    }

    async fn getattr(&self, handle: &[u8]) -> Result<Attr, RemoteError> {
        let reply = self
            .nfs_call(GETATTR, Xdr::default().opaque(handle))
            .await?;
        let mut i = In::new(&reply);
        let status = i.u32();
        if status != 0 {
            return Err(status_error("reading a file's details", status));
        }
        let a = fattr3(&mut i);
        if i.bad {
            return Err(RemoteError::Io("NFS: malformed reply".to_string()));
        }
        Ok(a)
    }

    /// A folder's entries, without `.` and `..`.
    async fn read_dir(&self, handle: &[u8]) -> Result<Vec<Entry>, RemoteError> {
        match self.read_dir_plus(handle).await {
            Err(RemoteError::Unsupported(_)) => self.read_dir_plain(handle).await,
            other => other,
        }
    }

    async fn read_dir_plus(&self, handle: &[u8]) -> Result<Vec<Entry>, RemoteError> {
        let mut out = Vec::new();
        let (mut cookie, mut verf) = (0u64, [0u8; 8]);
        loop {
            let before = cookie;
            let args = Xdr::default()
                .opaque(handle)
                .u64(cookie)
                .fixed(&verf)
                .u32(1 << 16)
                .u32(1 << 20);
            let reply = self.nfs_call(READDIRPLUS, args).await?;
            let mut i = In::new(&reply);
            let status = i.u32();
            if status == NOT_SUPPORTED {
                return Err(RemoteError::Unsupported("READDIRPLUS".to_string()));
            }
            if status != 0 {
                return Err(status_error("listing a folder", status));
            }
            post_op_attr(&mut i);
            verf.copy_from_slice(i.take(8).get(..8).unwrap_or(&[0; 8]));
            while i.u32() != 0 && !i.bad {
                i.u64(); // fileid
                let name = i.text();
                cookie = i.u64();
                let attr = post_op_attr(&mut i);
                if i.u32() != 0 {
                    i.opaque(64);
                }
                if name != "." && name != ".." {
                    // Linux leaves these out for an entry that is another mount.
                    let a = match attr {
                        Some(a) => a,
                        None => {
                            let (h, found) = self.lookup(handle, &name).await?;
                            match found {
                                Some(a) => a,
                                None => self.getattr(&h).await?,
                            }
                        }
                    };
                    out.push(Entry {
                        name,
                        is_dir: a.kind == TYPE_DIR,
                        size: a.size,
                        mtime: (a.mtime != 0).then_some(a.mtime),
                    });
                }
            }
            let eof = i.u32() != 0;
            if i.bad {
                return Err(RemoteError::Io("NFS: malformed listing".to_string()));
            }
            if eof {
                return Ok(out);
            }
            if cookie == before || out.len() > MAX_LISTED {
                return Err(RemoteError::Io(
                    "NFS: a folder listing doesn't end".to_string(),
                ));
            }
        }
    }

    async fn read_dir_plain(&self, handle: &[u8]) -> Result<Vec<Entry>, RemoteError> {
        let mut names = Vec::new();
        let (mut cookie, mut verf) = (0u64, [0u8; 8]);
        loop {
            let before = cookie;
            let args = Xdr::default()
                .opaque(handle)
                .u64(cookie)
                .fixed(&verf)
                .u32(1 << 16);
            let reply = self.nfs_call(READDIR, args).await?;
            let mut i = In::new(&reply);
            let status = i.u32();
            if status != 0 {
                return Err(status_error("listing a folder", status));
            }
            post_op_attr(&mut i);
            verf.copy_from_slice(i.take(8).get(..8).unwrap_or(&[0; 8]));
            while i.u32() != 0 && !i.bad {
                i.u64();
                let name = i.text();
                cookie = i.u64();
                if name != "." && name != ".." {
                    names.push(name);
                }
            }
            let eof = i.u32() != 0;
            if i.bad {
                return Err(RemoteError::Io("NFS: malformed listing".to_string()));
            }
            if eof {
                break;
            }
            if cookie == before || names.len() > MAX_LISTED {
                return Err(RemoteError::Io(
                    "NFS: a folder listing doesn't end".to_string(),
                ));
            }
        }
        let mut out = Vec::with_capacity(names.len());
        for name in names {
            let (h, attr) = self.lookup(handle, &name).await?;
            let a = match attr {
                Some(a) => a,
                None => self.getattr(&h).await?,
            };
            out.push(Entry {
                name,
                is_dir: a.kind == TYPE_DIR,
                size: a.size,
                mtime: (a.mtime != 0).then_some(a.mtime),
            });
        }
        Ok(out)
    }

    async fn entries(&self, path: &str) -> Result<Vec<Entry>, RemoteError> {
        let Some((export, rel)) = self.place(path).await? else {
            // Above the exports: the next step down each of them.
            let all = self.exports().await?;
            return Ok(export_children(&all, path)
                .into_iter()
                .map(|name| Entry {
                    name,
                    is_dir: true,
                    size: 0,
                    mtime: None,
                })
                .collect());
        };
        let handle = self.resolve(&export, &rel).await?;
        let listed = self.read_dir(&handle).await;
        // A folder moved or deleted on the server since it was looked up.
        if matches!(&listed, Err(RemoteError::Io(m)) if m.contains(&status_text(STALE))) {
            self.forget_handles().await;
            let handle = self.resolve(&export, &rel).await?;
            return self.read_dir(&handle).await;
        }
        listed
    }

    async fn read_at(&self, handle: &[u8], offset: u64, len: u64) -> Result<Vec<u8>, RemoteError> {
        let chunk = u64::from(*self.read_size.lock().await);
        let mut out = Vec::with_capacity(len as usize);
        while (out.len() as u64) < len {
            let at = offset + out.len() as u64;
            let want = (len - out.len() as u64).min(chunk) as u32;
            let args = Xdr::default().opaque(handle).u64(at).u32(want);
            let reply = self.nfs_call(READ, args).await?;
            let mut i = In::new(&reply);
            let status = i.u32();
            if status != 0 {
                return Err(status_error("reading a file", status));
            }
            post_op_attr(&mut i);
            i.u32(); // count
            let eof = i.u32() != 0;
            let data = i.opaque(MAX_RECORD);
            if i.bad {
                return Err(RemoteError::Io("NFS: malformed read reply".to_string()));
            }
            let empty = data.is_empty();
            out.extend_from_slice(&data);
            if eof || empty {
                break;
            }
        }
        Ok(out)
    }
}

/// `sattr3` with a mode, or a size of 0 too for a file made new.
fn sattr(mode: u32, truncate: bool) -> Xdr {
    let x = Xdr::default().u32(1).u32(mode).u32(0).u32(0);
    let x = if truncate { x.u32(1).u64(0) } else { x.u32(0) };
    x.u32(0).u32(0)
}

pub struct NfsFs {
    inner: Arc<Inner>,
}

impl NfsFs {
    pub async fn connect(conn: &Connection) -> Result<Self, RemoteError> {
        let (uid, gid) = parse_ids(&conn.user)?;
        let share = conn.share.trim().trim_matches('/');
        let root = if share.is_empty() {
            String::new()
        } else {
            format!("/{share}")
        };
        let host = conn
            .host
            .trim()
            .trim_start_matches('[')
            .trim_end_matches(']');
        let nfs = Rpc::open(host, conn.port, NFS, 3, uid, gid).await?;
        let inner = Arc::new(Inner {
            host: host.to_string(),
            port: conn.port,
            uid,
            gid,
            root,
            state: Mutex::new(State {
                nfs,
                mount_port: None,
                exports: None,
                roots: HashMap::new(),
                handles: HashMap::new(),
            }),
            read_size: Mutex::new(1 << 16),
            write_size: Mutex::new(1 << 16),
        });
        // Signing in = reaching the start folder (or the export, or the list of them).
        let start = conn.start_path.trim();
        let fs = Self { inner };
        fs.inner
            .entries(if start.is_empty() { "/" } else { start })
            .await?;
        if !fs.inner.root.is_empty() {
            let handle = fs.inner.mount(&fs.inner.root).await?;
            fs.inner.read_limits(&handle).await;
        }
        Ok(fs)
    }
}

struct NfsFile {
    inner: Arc<Inner>,
    handle: Vec<u8>,
    size: u64,
}

#[async_trait::async_trait]
impl RemoteFile for NfsFile {
    fn size(&self) -> u64 {
        self.size
    }
    async fn read_at(&self, offset: u64, len: u64) -> Result<Vec<u8>, RemoteError> {
        if offset >= self.size || len == 0 {
            return Ok(Vec::new());
        }
        let len = len.min(self.size - offset);
        self.inner.read_at(&self.handle, offset, len).await
    }
}

#[async_trait::async_trait]
impl RemoteFs for NfsFs {
    async fn list(&self, path: &str, _cursor: Option<String>) -> Result<Page, RemoteError> {
        Ok(Page {
            entries: self.inner.entries(path).await?,
            next_cursor: None,
        })
    }

    async fn stat(&self, path: &str) -> Result<Entry, RemoteError> {
        let parts = split(path);
        let name = parts.last().copied().unwrap_or("/").to_string();
        if self.inner.place(path).await?.is_none() {
            return Ok(Entry {
                name,
                is_dir: true,
                size: 0,
                mtime: None,
            });
        }
        let handle = self.inner.handle_of(path).await?;
        let a = self.inner.getattr(&handle).await?;
        Ok(Entry {
            name,
            is_dir: a.kind == TYPE_DIR,
            size: a.size,
            mtime: (a.mtime != 0).then_some(a.mtime),
        })
    }

    async fn open(&self, path: &str) -> Result<Arc<dyn RemoteFile>, RemoteError> {
        let handle = self.inner.handle_of(path).await?;
        let a = self.inner.getattr(&handle).await?;
        if a.kind == TYPE_DIR {
            return Err(RemoteError::Io(format!("{path} is a folder")));
        }
        Ok(Arc::new(NfsFile {
            inner: Arc::clone(&self.inner),
            handle,
            size: a.size,
        }))
    }

    async fn walk_until(
        &self,
        path: &str,
        limit: usize,
        cancel: &std::sync::atomic::AtomicBool,
    ) -> Result<Vec<(String, Entry)>, RemoteError> {
        let mut out = Vec::new();
        let mut stack = vec![String::new()];
        while let Some(rel) = stack.pop() {
            if cancel.load(std::sync::atomic::Ordering::Relaxed) {
                return Err(super::walk_cancelled());
            }
            let dir = if rel.is_empty() {
                path.to_string()
            } else {
                format!("{}/{rel}", path.trim_end_matches('/'))
            };
            for e in self.inner.entries(&dir).await? {
                let child = if rel.is_empty() {
                    e.name.clone()
                } else {
                    format!("{rel}/{}", e.name)
                };
                if e.is_dir {
                    stack.push(child);
                } else {
                    out.push((child, e));
                    if out.len() >= limit {
                        return Ok(out);
                    }
                }
            }
        }
        Ok(out)
    }

    async fn create_dir(&self, path: &str) -> Result<(), RemoteError> {
        let (dir, name) = self.inner.parent_of(path).await?;
        let args = Xdr::default().dir_op(&dir, &name);
        let mut args = args;
        args.0.extend_from_slice(&sattr(0o755, false).0);
        let reply = self.inner.nfs_call(MKDIR, args).await?;
        let status = In::new(&reply).u32();
        match status {
            0 => Ok(()),
            // Something is there already: a folder is fine, a file is not.
            17 => {
                if self.stat(path).await?.is_dir {
                    Ok(())
                } else {
                    Err(RemoteError::Exists(format!("{path} is a file")))
                }
            }
            s => Err(status_error(path, s)),
        }
    }

    async fn rename(&self, from: &str, to: &str) -> Result<(), RemoteError> {
        // NFS's RENAME replaces what it lands on, which this must never do.
        match self.stat(to).await {
            Ok(_) => return Err(RemoteError::Exists(format!("{to} already exists"))),
            Err(RemoteError::NotFound(_)) => {}
            Err(e) => return Err(e),
        }
        let (from_dir, from_name) = self.inner.parent_of(from).await?;
        let (to_dir, to_name) = self.inner.parent_of(to).await?;
        let args = Xdr::default()
            .dir_op(&from_dir, &from_name)
            .dir_op(&to_dir, &to_name);
        let reply = self.inner.nfs_call(RENAME, args).await?;
        self.inner.forget_handles().await;
        match In::new(&reply).u32() {
            0 => Ok(()),
            s => Err(status_error(from, s)),
        }
    }

    async fn remove_dir(&self, path: &str) -> Result<(), RemoteError> {
        let (dir, name) = self.inner.parent_of(path).await?;
        let reply = self
            .inner
            .nfs_call(RMDIR, Xdr::default().dir_op(&dir, &name))
            .await?;
        self.inner.forget_handles().await;
        match In::new(&reply).u32() {
            0 => Ok(()),
            s => Err(status_error(path, s)),
        }
    }

    async fn remove_file(&self, path: &str) -> Result<(), RemoteError> {
        let (dir, name) = self.inner.parent_of(path).await?;
        let reply = self
            .inner
            .nfs_call(REMOVE, Xdr::default().dir_op(&dir, &name))
            .await?;
        self.inner.forget_handles().await;
        match In::new(&reply).u32() {
            0 => Ok(()),
            s => Err(status_error(path, s)),
        }
    }

    /// CREATE (UNCHECKED, size 0) and WRITEs at their offsets, UNSTABLE, with one COMMIT at the
    /// end. From a byte past the start the file is looked up and written on from there.
    async fn create_writer<'a>(
        &'a self,
        path: &str,
        _size: u64,
        from: u64,
    ) -> Result<Box<dyn super::RemoteWriter + 'a>, RemoteError> {
        let (dir, name) = self.inner.parent_of(path).await?;
        let handle = if from == 0 {
            let mut args = Xdr::default().dir_op(&dir, &name).u32(0); // UNCHECKED
            args.0.extend_from_slice(&sattr(0o644, true).0);
            let reply = self.inner.nfs_call(CREATE, args).await?;
            let mut i = In::new(&reply);
            let status = i.u32();
            if status != 0 {
                return Err(status_error(path, status));
            }
            if i.u32() != 0 {
                i.opaque(64)
            } else {
                self.inner.lookup(&dir, &name).await?.0
            }
        } else {
            let (handle, attr) = self.inner.lookup(&dir, &name).await?;
            let size = match attr {
                Some(a) => a.size,
                None => self.inner.getattr(&handle).await?.size,
            };
            if size < from {
                return Err(RemoteError::NotFound(format!(
                    "{path} holds {size} bytes, not the {from} to go on from"
                )));
            }
            handle
        };
        let chunk = (*self.inner.write_size.lock().await).min(super::PUT_CHUNK as u32) as usize;
        Ok(Box::new(NfsWriter {
            inner: Arc::clone(&self.inner),
            handle,
            path: path.to_string(),
            at: from,
            chunk,
            verifier: None,
        }))
    }

    fn writes_from(&self) -> bool {
        true
    }

    /// FSSTAT's `abytes`: what this user may still write on the export.
    async fn free_space(&self, path: &str) -> Result<Option<u64>, RemoteError> {
        if self.inner.place(path).await?.is_none() {
            return Ok(None);
        }
        let handle = self.inner.handle_of(path).await?;
        let reply = self
            .inner
            .nfs_call(FSSTAT, Xdr::default().opaque(&handle))
            .await?;
        let mut i = In::new(&reply);
        let status = i.u32();
        if status != 0 {
            return Err(status_error(path, status));
        }
        post_op_attr(&mut i);
        let (_total, _free, avail) = (i.u64(), i.u64(), i.u64());
        if i.bad {
            return Err(RemoteError::Io("NFS: malformed FSSTAT reply".to_string()));
        }
        Ok(Some(avail))
    }
}

/// A file being written on an export. Every call runs as its own task: a write given up part
/// way (a cancel, a stall) never leaves half a call on the shared connection.
struct NfsWriter {
    inner: Arc<Inner>,
    handle: Vec<u8>,
    path: String,
    at: u64,
    chunk: usize,
    /// The server's write verifier: a different one later means it restarted and may have
    /// lost UNSTABLE writes.
    verifier: Option<Vec<u8>>,
}

impl NfsWriter {
    async fn call(&self, proc: u32, args: Xdr) -> Result<Vec<u8>, RemoteError> {
        let inner = Arc::clone(&self.inner);
        tokio::spawn(async move { inner.nfs_call(proc, args).await })
            .await
            .map_err(|e| RemoteError::Io(format!("NFS: {e}")))?
    }

    fn restarted(&self) -> RemoteError {
        RemoteError::Io(format!(
            "NFS: the server restarted while writing {}; send it again",
            self.path
        ))
    }
}

#[async_trait::async_trait]
impl super::RemoteWriter for NfsWriter {
    async fn write(&mut self, data: &[u8]) -> Result<(), RemoteError> {
        for piece in data.chunks(self.chunk.max(1)) {
            let args = Xdr::default()
                .opaque(&self.handle)
                .u64(self.at)
                .u32(piece.len() as u32)
                .u32(0) // UNSTABLE, committed at the end
                .opaque(piece);
            let reply = self.call(WRITE, args).await?;
            let mut i = In::new(&reply);
            let status = i.u32();
            if status != 0 {
                return Err(status_error(&self.path, status));
            }
            wcc_data(&mut i);
            let wrote = i.u32() as usize;
            i.u32(); // committed
            let verf = i.take(8).to_vec();
            if i.bad || wrote != piece.len() {
                return Err(RemoteError::Io(format!(
                    "NFS: a short write to {}: {wrote} of {} bytes",
                    self.path,
                    piece.len()
                )));
            }
            if self.verifier.get_or_insert_with(|| verf.clone()) != &verf {
                return Err(self.restarted());
            }
            self.at += piece.len() as u64;
        }
        Ok(())
    }

    async fn finish(self: Box<Self>) -> Result<(), RemoteError> {
        let args = Xdr::default().opaque(&self.handle).u64(0).u32(0);
        let reply = self.call(COMMIT, args).await?;
        let mut i = In::new(&reply);
        let status = i.u32();
        if status != 0 {
            return Err(status_error(&self.path, status));
        }
        wcc_data(&mut i);
        let verf = i.take(8).to_vec();
        if self.verifier.as_ref().is_some_and(|v| *v != verf) {
            return Err(self.restarted());
        }
        Ok(())
    }

    /// Nothing is held open: NFS has no open files.
    async fn abort(self: Box<Self>) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Against a real server: `NFS_TEST=host:port`, a tree with `games/big.bin`, writable. rclone's
    /// `serve nfs` in Docker does. Reads, a write that reads back, and the refusals.
    #[tokio::test]
    #[ignore = "needs an NFS server"]
    async fn against_a_real_server() {
        use sha2::{Digest, Sha256};
        let target = std::env::var("NFS_TEST").expect("NFS_TEST=host:port");
        let (host, port) = target.split_once(':').unwrap();
        let mut conn = crate::remote::store::conn("t", crate::remote::store::Protocol::Nfs);
        conn.host = host.to_string();
        conn.port = port.parse().unwrap();
        conn.user = "1000:1000".to_string();
        conn.share = String::new();
        conn.start_path = String::new();
        let fs = NfsFs::connect(&conn).await.unwrap();
        let listed = fs.list("/", None).await.unwrap();
        assert!(listed.entries.iter().any(|e| e.name == "games" && e.is_dir));
        let f = fs.open("/games/big.bin").await.unwrap();
        let all = f.read_at(0, f.size()).await.unwrap();
        assert_eq!(all.len() as u64, f.size());
        let middle = f.read_at(70_000, 9).await.unwrap();
        assert_eq!(middle, all[70_000..70_009]);
        let sum: String = Sha256::digest(&all)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        println!("big.bin sha256 {sum}");
        assert!(f.read_at(f.size(), 9).await.unwrap().is_empty());
        assert!(matches!(
            fs.stat("/games/nope").await,
            Err(RemoteError::NotFound(_))
        ));

        fs.create_dir("/t").await.unwrap();
        fs.create_dir("/t").await.unwrap();
        let src = std::env::temp_dir().join(format!("nfs-put-{}", std::process::id()));
        std::fs::write(&src, &all[..3_000_000]).unwrap();
        let sent = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let add = Arc::clone(&sent);
        let progress = crate::remote::PutProgress {
            add: Box::new(move |n| {
                add.fetch_add(n, std::sync::atomic::Ordering::Relaxed);
            }),
            cancel: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        };
        fs.put_file("/t/a.bin", &src, &progress).await.unwrap();
        assert_eq!(sent.load(std::sync::atomic::Ordering::Relaxed), 3_000_000);
        let back = fs.open("/t/a.bin").await.unwrap();
        assert_eq!(
            back.read_at(0, back.size()).await.unwrap(),
            all[..3_000_000]
        );
        fs.put_file("/t/b.bin", &src, &progress).await.unwrap();
        assert!(matches!(
            fs.rename("/t/a.bin", "/t/b.bin").await,
            Err(RemoteError::Exists(_))
        ));
        fs.rename("/t/a.bin", "/t/c.bin").await.unwrap();
        assert!(fs.stat("/t/c.bin").await.is_ok());
        assert!(matches!(fs.remove_dir("/t").await, Err(RemoteError::Io(_))));
        fs.remove_file("/t/b.bin").await.unwrap();
        fs.remove_file("/t/c.bin").await.unwrap();
        fs.remove_dir("/t").await.unwrap();
        let walked = fs.walk("/games", 10).await.unwrap();
        assert!(walked.iter().any(|(p, _)| p == "big.bin"));
        let _ = std::fs::remove_file(&src);
    }

    #[test]
    fn exports_make_a_tree() {
        let all = vec![
            "/volume1/games".to_string(),
            "/volume1/media".to_string(),
            "/srv".to_string(),
        ];
        assert_eq!(export_children(&all, "/"), ["srv", "volume1"]);
        assert_eq!(export_children(&all, "/volume1"), ["games", "media"]);
        assert!(export_children(&all, "/volume1/games").is_empty());
        assert_eq!(
            export_of(&all, "/volume1/games/a/b").as_deref(),
            Some("/volume1/games")
        );
        assert_eq!(export_of(&all, "/volume1"), None, "above the exports");
        let root = vec!["/".to_string(), "/deep/er".to_string()];
        assert_eq!(export_of(&root, "/deep/er/x").as_deref(), Some("/deep/er"));
        assert_eq!(export_of(&root, "/deep").as_deref(), Some("/"));
    }

    #[test]
    fn ids_read_as_a_user_and_group() {
        assert_eq!(parse_ids("").unwrap(), (0, 0));
        assert_eq!(parse_ids("1000").unwrap(), (1000, 1000));
        assert_eq!(parse_ids(" 1000:100 ").unwrap(), (1000, 100));
        assert!(matches!(parse_ids("admin"), Err(RemoteError::Auth(_))));
    }

    #[test]
    fn a_reply_is_matched_to_its_call() {
        let reply = |xid: u32, stat: u32, accept: u32| {
            Xdr::default()
                .u32(xid)
                .u32(1)
                .u32(stat)
                .u32(0)
                .u32(0)
                .u32(accept)
                .u32(7)
                .0
        };
        assert_eq!(
            parse_reply(&reply(1, 0, 0), 1).unwrap().unwrap(),
            7u32.to_be_bytes()
        );
        assert!(parse_reply(&reply(2, 0, 0), 1).unwrap().is_none());
        assert!(matches!(
            parse_reply(&reply(1, 0, 2), 1),
            Err(RemoteError::Io(m)) if m.contains("version 3")
        ));
        let denied = Xdr::default().u32(1).u32(1).u32(1).u32(1).0;
        assert!(matches!(parse_reply(&denied, 1), Err(RemoteError::Auth(_))));
    }

    #[test]
    fn a_short_reply_is_noticed_not_read_past() {
        let mut i = In::new(&[0, 0, 0, 9, 1]);
        assert_eq!(i.opaque(100), Vec::<u8>::new());
        assert!(i.bad);
        let padded = Xdr::default().opaque(b"abcde").u32(3).0;
        let mut i = In::new(&padded);
        assert_eq!(i.opaque(100), b"abcde");
        assert_eq!(i.u32(), 3);
        assert!(!i.bad);
    }

    #[test]
    fn a_status_becomes_the_right_kind_of_failure() {
        assert!(matches!(status_error("x", 2), RemoteError::NotFound(_)));
        assert!(matches!(status_error("x", 13), RemoteError::Auth(_)));
        assert!(matches!(status_error("x", 17), RemoteError::Exists(_)));
        assert!(matches!(status_error("x", 28), RemoteError::Io(m) if m.contains("disk is full")));
    }
}
