//! The engine's own tailnet node: a safe wrapper over the C core in `tailnet/` (control,
//! DERP, disco/STUN direct paths, WireGuard, lwIP). Userspace sockets only: no OS VPN, no
//! Tailscale app, nothing to install.
//!
//! The engine reaches a console on the tailnet through *mappings*: [`Node::map`] opens a
//! port on 127.0.0.1 (TCP listener or UDP socket) whose traffic the core carries to the
//! console's tailnet address and port, so the rest of the engine keeps using ordinary
//! sockets.
//!
//! One node per process (the core's lwIP keeps global state).

use std::ffi::{c_char, c_int, c_void, CStr, CString};
use std::net::Ipv4Addr;
use std::path::Path;
use std::sync::Arc;

mod ffi {
    use super::*;

    pub const TN_NAME_MAX: usize = 64;

    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    pub struct Ep4 {
        pub ip: u32,
        pub port: u16,
    }

    #[repr(C)]
    pub struct Event {
        pub kind: c_int,
        pub state: c_int,
        pub err: c_int,
        pub text: *const c_char,
        pub netmap: *const c_void,
    }

    pub type EventCb = Option<unsafe extern "C" fn(user: *mut c_void, ev: *const Event)>;
    pub type PacketCb = Option<unsafe extern "C" fn(user: *mut c_void, pkt: *const u8, len: usize)>;
    pub type ThreadCb = Option<unsafe extern "C" fn(user: *mut c_void, role: *const c_char)>;

    #[repr(C)]
    pub struct Config {
        pub state_dir: *const c_char,
        pub control_url: *const c_char,
        pub auth_key: *const c_char,
        pub hostname: *const c_char,
        pub os: *const c_char,
        pub app: *const c_char,
        pub ca_file: *const c_char,
        pub control_addr: *const c_char,
        pub transport: c_int,
        pub ephemeral: c_int,
        pub verbose: c_int,
        pub cb: EventCb,
        pub user: *mut c_void,
        pub max_peers: u32,
        pub max_regions: u32,
        pub max_rules: u32,
        pub map_msg_max: u32,
        pub json_tokens_max: u32,
        pub dump_dir: *const c_char,
        pub udp_port: u16,
        pub packet_cb: PacketCb,
        pub no_direct: c_int,
        pub serve: *const c_void,
        pub n_serve: u32,
        pub thread_cb: ThreadCb,
        pub thread_stack: usize,
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    pub struct PeerStatus {
        pub name: [c_char; TN_NAME_MAX],
        pub addr4: u32,
        pub online: u8,
        pub handshake: u8,
        pub path: c_int,
        pub direct: Ep4,
        pub derp_region: i32,
        pub latency_us: i32,
        pub last_rx_ms: i64,
        pub tx_bytes: u64,
        pub rx_bytes: u64,
    }

    #[repr(C)]
    pub struct Status {
        pub state: c_int,
        pub self_addr4: u32,
        pub self_name: [c_char; TN_NAME_MAX],
        pub home_derp: i32,
        pub home_latency_us: i32,
        pub derp_connected: u8,
        pub udp_port: u16,
        pub n_endpoints: u32,
        pub endpoints: [Ep4; 8],
        pub key_expiry: i64,
        pub n_peers: u32,
        pub fwd_udp_flows: u32,
        pub fwd_tcp_conns: u32,
        pub login_url: [c_char; 256],
    }

    pub enum Ctx {}

    extern "C" {
        pub fn tn_create(cfg: *const Config, out: *mut *mut Ctx) -> c_int;
        pub fn tn_start(ctx: *mut Ctx) -> c_int;
        pub fn tn_pause(ctx: *mut Ctx);
        pub fn tn_resume(ctx: *mut Ctx);
        pub fn tn_reauth(ctx: *mut Ctx) -> c_int;
        pub fn tn_destroy(ctx: *mut Ctx);
        pub fn tn_map(
            ctx: *mut Ctx,
            proto: u8,
            rip: u32,
            rport: u16,
            want: u16,
            lport: *mut u16,
        ) -> c_int;
        pub fn tn_unmap(ctx: *mut Ctx, proto: u8, lport: u16);
        pub fn tn_get_status(
            ctx: *mut Ctx,
            st: *mut Status,
            peers: *mut PeerStatus,
            cap: u32,
        ) -> c_int;
        pub fn tn_strerror(err: c_int) -> *const c_char;
        pub fn tn_state_name(st: c_int) -> *const c_char;
        pub fn tn_abi_size(which: c_int) -> usize;
    }
}

/// A failure from the core (`TN_E_*`), with its text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error {
    pub code: i32,
    pub text: String,
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "tailnet: {} ({})", self.text, self.code)
    }
}

impl std::error::Error for Error {}

fn err(code: c_int) -> Error {
    // SAFETY: tn_strerror returns a static NUL-terminated string for every code.
    let text = unsafe { CStr::from_ptr(ffi::tn_strerror(code)) }
        .to_string_lossy()
        .into_owned();
    Error { code, text }
}

/// What the node tells the app, on the core's threads (keep the handler short).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    State(String),
    LoginUrl(String),
    NetmapChanged,
    Log(String),
    Error(String),
    Path(String),
    Warning(String),
}

#[derive(Debug, Clone, Default)]
pub struct Config {
    /// Keys and the control key cache live here (created if missing).
    pub state_dir: std::path::PathBuf,
    pub control_url: String,
    /// A pre-auth key for an unattended sign-in; empty for the login URL.
    pub auth_key: String,
    pub hostname: String,
    /// Hostinfo.OS: "macOS", "windows", "linux", "android", "iOS".
    pub os: String,
    /// Extra PEM trust anchors for a private control server (Headscale with its own CA).
    pub ca_file: Option<std::path::PathBuf>,
    /// UDP port for WireGuard/disco/STUN; 0 = 41641 (another free port if taken).
    pub udp_port: u16,
    /// Debug: DERP only.
    pub no_direct: bool,
    /// TN_EV_LOG detail: 0 quiet, 1 info, 2 debug.
    pub verbose: i32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Proto {
    Tcp,
    Udp,
}

impl Proto {
    fn num(self) -> u8 {
        match self {
            Proto::Tcp => 6,
            Proto::Udp => 17,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PathKind {
    Idle,
    Relayed,
    Direct,
}

#[derive(Debug, Clone)]
pub struct PeerStatus {
    pub name: String,
    pub ip: Option<Ipv4Addr>,
    pub online: bool,
    pub handshake: bool,
    pub path: PathKind,
    /// "a.b.c.d:port" when direct.
    pub direct: Option<String>,
    pub derp_region: i32,
    pub latency_ms: Option<f64>,
    pub last_rx_ms: Option<i64>,
}

#[derive(Debug, Clone)]
pub struct Status {
    /// "connecting", "needs-login", "running", ... (tn_state_name).
    pub state: String,
    pub ip: Option<Ipv4Addr>,
    pub name: String,
    pub home_derp: i32,
    pub home_latency_ms: Option<f64>,
    pub derp_connected: bool,
    pub udp_port: u16,
    pub endpoints: Vec<String>,
    pub key_expiry: i64,
    pub login_url: String,
    pub peers: Vec<PeerStatus>,
}

fn cstr_arr(a: &[c_char]) -> String {
    let bytes: Vec<u8> = a
        .iter()
        .take_while(|&&c| c != 0)
        .map(|&c| c as u8)
        .collect();
    String::from_utf8_lossy(&bytes).into_owned()
}

fn ip_of(a: u32) -> Option<Ipv4Addr> {
    (a != 0).then(|| Ipv4Addr::from(a))
}

type Handler = dyn Fn(Event) + Send + Sync + 'static;

struct Callback {
    handler: Arc<Handler>,
}

unsafe extern "C" fn event_trampoline(user: *mut c_void, ev: *const ffi::Event) {
    if user.is_null() || ev.is_null() {
        return;
    }
    // SAFETY: user is the Box<Callback> the Node owns for the context's whole life; ev and
    // its text are valid for the duration of this call (tailnet.h).
    let (cb, ev) = unsafe { (&*(user as *const Callback), &*ev) };
    let text = if ev.text.is_null() {
        String::new()
    } else {
        unsafe { CStr::from_ptr(ev.text) }
            .to_string_lossy()
            .into_owned()
    };
    let event = match ev.kind {
        1 => Event::State(text),
        2 => Event::LoginUrl(text),
        3 => Event::NetmapChanged,
        4 => Event::Log(text),
        5 => Event::Error(text),
        6 => Event::Path(text),
        7 => Event::Warning(text),
        _ => return,
    };
    let handler = cb.handler.clone();
    // A panic must not unwind into C.
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| handler(event)));
}

/// A running node. Dropping it leaves the tailnet (the keys stay in `state_dir`).
pub struct Node {
    ctx: *mut ffi::Ctx,
    _cb: Box<Callback>,
}

// SAFETY: every tn_* call is thread-safe (tailnet.h: "may be called from any thread"),
// tn_destroy runs once, in Drop.
unsafe impl Send for Node {}
unsafe impl Sync for Node {}

fn cs(s: &str) -> Result<CString, Error> {
    CString::new(s).map_err(|_| Error {
        code: -11,
        text: "a NUL byte in a setting".into(),
    })
}

impl Node {
    /// Create the node and start its threads. Signs in with `auth_key` or issues a login URL
    /// ([`Event::LoginUrl`], also in [`Node::status`]).
    pub fn start(
        cfg: &Config,
        on_event: impl Fn(Event) + Send + Sync + 'static,
    ) -> Result<Node, Error> {
        check_abi()?;
        let dir = cs(&cfg.state_dir.to_string_lossy())?;
        let url = cs(&cfg.control_url)?;
        let key = cs(&cfg.auth_key)?;
        let host = cs(&cfg.hostname)?;
        let os = cs(&cfg.os)?;
        let app = cs("ps5upload")?;
        let ca = match &cfg.ca_file {
            Some(p) => Some(cs(&p.to_string_lossy())?),
            None => None,
        };
        let cb = Box::new(Callback {
            handler: Arc::new(on_event),
        });
        let c = ffi::Config {
            state_dir: dir.as_ptr(),
            control_url: url.as_ptr(),
            auth_key: if cfg.auth_key.is_empty() {
                std::ptr::null()
            } else {
                key.as_ptr()
            },
            hostname: if cfg.hostname.is_empty() {
                std::ptr::null()
            } else {
                host.as_ptr()
            },
            os: if cfg.os.is_empty() {
                std::ptr::null()
            } else {
                os.as_ptr()
            },
            app: app.as_ptr(),
            ca_file: ca.as_ref().map_or(std::ptr::null(), |c| c.as_ptr()),
            control_addr: std::ptr::null(),
            transport: 0,
            ephemeral: 0,
            verbose: cfg.verbose,
            cb: Some(event_trampoline),
            user: &*cb as *const Callback as *mut c_void,
            max_peers: 0,
            max_regions: 0,
            max_rules: 0,
            // A phone needs less than the desktop default (4 MiB): a home tailnet.
            map_msg_max: 1 << 20,
            json_tokens_max: 16384,
            dump_dir: std::ptr::null(),
            udp_port: cfg.udp_port,
            packet_cb: None,
            no_direct: cfg.no_direct as c_int,
            serve: std::ptr::null(),
            n_serve: 0,
            thread_cb: None,
            thread_stack: 0,
        };
        let mut ctx: *mut ffi::Ctx = std::ptr::null_mut();
        // SAFETY: c and the strings it points to outlive the call; tn_create copies them.
        let r = unsafe { ffi::tn_create(&c, &mut ctx) };
        if r != 0 {
            return Err(err(r));
        }
        let node = Node { ctx, _cb: cb };
        // SAFETY: ctx is valid until Drop.
        let r = unsafe { ffi::tn_start(node.ctx) };
        if r != 0 {
            return Err(err(r));
        }
        Ok(node)
    }

    /// A port on 127.0.0.1 that reaches `remote:port` on the tailnet. The same mapping again
    /// returns the same port. `want` 0 = any free port.
    pub fn map(&self, proto: Proto, remote: Ipv4Addr, port: u16, want: u16) -> Result<u16, Error> {
        let mut lport = 0u16;
        // SAFETY: ctx is valid until Drop; lport is a valid out pointer.
        let r = unsafe {
            ffi::tn_map(
                self.ctx,
                proto.num(),
                u32::from(remote),
                port,
                want,
                &mut lport,
            )
        };
        if r != 0 {
            return Err(err(r));
        }
        Ok(lport)
    }

    /// Close every mapping (and the connections through them).
    pub fn unmap_all(&self) {
        // SAFETY: ctx is valid until Drop.
        unsafe { ffi::tn_unmap(self.ctx, 0, 0) }
    }

    /// Rest/sleep of the host: drop connections; [`Node::resume`] re-establishes them.
    pub fn pause(&self) {
        unsafe { ffi::tn_pause(self.ctx) }
    }

    pub fn resume(&self) {
        unsafe { ffi::tn_resume(self.ctx) }
    }

    /// Renew the sign-in keeping this node (its address and name): a new login URL follows.
    pub fn reauth(&self) -> Result<(), Error> {
        // SAFETY: ctx is valid until Drop.
        let r = unsafe { ffi::tn_reauth(self.ctx) };
        if r != 0 {
            return Err(err(r));
        }
        Ok(())
    }

    pub fn status(&self) -> Status {
        let mut st: ffi::Status = unsafe { std::mem::zeroed() };
        let mut peers: Vec<ffi::PeerStatus> = vec![unsafe { std::mem::zeroed() }; 256];
        // SAFETY: st and peers are valid for the sizes given.
        unsafe { ffi::tn_get_status(self.ctx, &mut st, peers.as_mut_ptr(), peers.len() as u32) };
        let n = (st.n_peers as usize).min(peers.len());
        let state = unsafe { CStr::from_ptr(ffi::tn_state_name(st.state)) }
            .to_string_lossy()
            .into_owned();
        Status {
            state,
            ip: ip_of(st.self_addr4),
            name: cstr_arr(&st.self_name),
            home_derp: st.home_derp,
            home_latency_ms: (st.home_latency_us >= 0).then(|| st.home_latency_us as f64 / 1000.0),
            derp_connected: st.derp_connected != 0,
            udp_port: st.udp_port,
            endpoints: st.endpoints[..(st.n_endpoints as usize).min(8)]
                .iter()
                .map(|e| format!("{}:{}", Ipv4Addr::from(e.ip), e.port))
                .collect(),
            key_expiry: st.key_expiry,
            login_url: cstr_arr(&st.login_url),
            peers: peers[..n]
                .iter()
                .map(|p| PeerStatus {
                    name: cstr_arr(&p.name),
                    ip: ip_of(p.addr4),
                    online: p.online != 0,
                    handshake: p.handshake != 0,
                    path: match p.path {
                        2 => PathKind::Direct,
                        1 => PathKind::Relayed,
                        _ => PathKind::Idle,
                    },
                    direct: (p.path == 2)
                        .then(|| format!("{}:{}", Ipv4Addr::from(p.direct.ip), p.direct.port)),
                    derp_region: p.derp_region,
                    latency_ms: (p.latency_us >= 0).then(|| p.latency_us as f64 / 1000.0),
                    last_rx_ms: (p.last_rx_ms >= 0).then_some(p.last_rx_ms),
                })
                .collect(),
        }
    }
}

impl Drop for Node {
    fn drop(&mut self) {
        // SAFETY: ctx came from tn_create and is destroyed exactly once.
        unsafe { ffi::tn_destroy(self.ctx) }
    }
}

/// The Rust mirrors of the C structs have the C sizes (a layout drift fails here, not as
/// memory corruption).
pub fn check_abi() -> Result<(), Error> {
    let want = [
        std::mem::size_of::<ffi::Config>(),
        std::mem::size_of::<ffi::Status>(),
        std::mem::size_of::<ffi::PeerStatus>(),
        std::mem::size_of::<ffi::Event>(),
    ];
    for (i, w) in want.iter().enumerate() {
        // SAFETY: a pure function.
        let c = unsafe { ffi::tn_abi_size(i as c_int) };
        if c != *w {
            return Err(Error {
                code: -11,
                text: format!("ABI mismatch for struct {i}: C {c}, Rust {w}"),
            });
        }
    }
    Ok(())
}

/// Hostinfo.OS for this build.
pub fn os_name() -> &'static str {
    if cfg!(target_os = "android") {
        "android"
    } else if cfg!(target_os = "ios") {
        "iOS"
    } else if cfg!(target_os = "macos") {
        "macOS"
    } else if cfg!(windows) {
        "windows"
    } else {
        "linux"
    }
}

/// Whether `dir` looks signed in (a key file exists); the node itself knows for sure.
pub fn has_keys(dir: &Path) -> bool {
    dir.join("tailnet-state.json").exists()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    #[test]
    fn abi_matches() {
        check_abi().unwrap();
    }

    #[test]
    fn starts_retries_and_maps_without_a_control_server() {
        let dir = std::env::temp_dir().join(format!("tailnet-rs-test-{}", std::process::id()));
        let (tx, rx) = mpsc::channel();
        let tx = std::sync::Mutex::new(tx);
        let node = Node::start(
            &Config {
                state_dir: dir.clone(),
                control_url: "http://127.0.0.1:9".into(),
                hostname: "rs-test".into(),
                os: os_name().into(),
                ..Default::default()
            },
            move |e| {
                let _ = tx.lock().unwrap().send(e);
            },
        )
        .unwrap();
        // The node runs and reports its state; control is a closed port, so it backs off.
        let first = rx.recv_timeout(std::time::Duration::from_secs(5)).unwrap();
        assert!(matches!(
            first,
            Event::State(_) | Event::Error(_) | Event::Log(_)
        ));
        assert!(has_keys(&dir));
        // A mapping opens a real local port even before the node has an address.
        let p = node
            .map(Proto::Tcp, Ipv4Addr::new(100, 64, 0, 9), 9120, 0)
            .unwrap();
        assert!(p > 0);
        assert_eq!(
            node.map(Proto::Tcp, Ipv4Addr::new(100, 64, 0, 9), 9120, 0)
                .unwrap(),
            p
        );
        let u = node
            .map(Proto::Udp, Ipv4Addr::new(100, 64, 0, 9), 9296, 0)
            .unwrap();
        assert!(u > 0);
        assert!(std::net::TcpStream::connect(("127.0.0.1", p)).is_ok());
        let st = node.status();
        assert!(st.ip.is_none());
        node.unmap_all();
        drop(node);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
