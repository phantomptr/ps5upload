//! The ffmpeg that Go live runs (#635, rp_golive.rs): downloaded on first use, never bundled.
//!
//! Go live sends the console's own video on unchanged and only turns its Opus sound into AAC
//! (streaming sites take no Opus). ffmpeg does that as a separate process, so its licence stays
//! its own (our code is GPL-3.0-or-later; these are ffmpeg's GPL builds).
//!
//! One build per OS and CPU is pinned below: its URL, size and SHA-256. The archive is hashed
//! as it downloads and nothing is unpacked, let alone run, unless the hash matches. Windows and
//! Linux builds are BtbN/FFmpeg-Builds (GitHub; its month-end autobuilds stay published for
//! about two years, so the pin is the 2026-09-30 one), macOS builds are Martin Riedl's
//! (ffmpeg.martin-riedl.de, release builds). All are ffmpeg 9.0.2.
//!
//! On disk: `<data>/tools/ffmpeg-9.0.2/bin/ffmpeg[.exe]` (plus its DLLs on Windows, `lib/` with
//! the shared libraries on Linux) and `installed.json`, which records the archive's hash and
//! the ffmpeg binary's own. The binary is hashed again (once per engine run) before it is first
//! run, so a file swapped after the install is refused rather than run.
//!
//! `PS5UPLOAD_FFMPEG=<path>`: a developer's own ffmpeg instead (tests, unusual systems); it is
//! the user's explicit choice, so it is not hashed.

use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

use sha2::{Digest, Sha256};

pub(crate) const FFMPEG_VERSION: &str = "9.0.2";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Archive {
    Zip,
    TarXz,
}

/// One pinned build.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Pin {
    /// `std::env::consts::OS` / `ARCH` it is for.
    pub os: &'static str,
    pub arch: &'static str,
    pub url: &'static str,
    pub sha256: &'static str,
    pub bytes: u64,
    pub archive: Archive,
}

pub(crate) const PINS: &[Pin] = &[
    Pin {
        os: "windows",
        arch: "x86_64",
        url: "https://github.com/BtbN/FFmpeg-Builds/releases/download/autobuild-2026-09-30-13-08/ffmpeg-n9.0.2-17-g2a571b6068-win64-gpl-shared-9.0.zip",
        sha256: "3da6c7b60bb9ccd73ec5b5e815ba804a0879eb362ba0e3beebce50174c022696",
        bytes: 86_321_582,
        archive: Archive::Zip,
    },
    Pin {
        os: "windows",
        arch: "aarch64",
        url: "https://github.com/BtbN/FFmpeg-Builds/releases/download/autobuild-2026-09-30-13-08/ffmpeg-n9.0.2-17-g2a571b6068-winarm64-gpl-shared-9.0.zip",
        sha256: "92460cf103340012d41413b557669902538f040e0c953abc1d719750dcd5f8a8",
        bytes: 60_050_693,
        archive: Archive::Zip,
    },
    Pin {
        os: "linux",
        arch: "x86_64",
        url: "https://github.com/BtbN/FFmpeg-Builds/releases/download/autobuild-2026-09-30-13-08/ffmpeg-n9.0.2-17-g2a571b6068-linux64-gpl-shared-9.0.tar.xz",
        sha256: "01a9764d0b5364b66cfeb4617557c64321b0e232e172ec73f0a701c5f7694326",
        bytes: 68_653_636,
        archive: Archive::TarXz,
    },
    Pin {
        os: "linux",
        arch: "aarch64",
        url: "https://github.com/BtbN/FFmpeg-Builds/releases/download/autobuild-2026-09-30-13-08/ffmpeg-n9.0.2-17-g2a571b6068-linuxarm64-gpl-shared-9.0.tar.xz",
        sha256: "706ccdbc8b0537646345532713a9b6750c0a6258cc001ec1c0379754f0ae730a",
        bytes: 57_679_524,
        archive: Archive::TarXz,
    },
    Pin {
        os: "macos",
        arch: "aarch64",
        url: "https://ffmpeg.martin-riedl.de/download/macos/arm64/1789931890_9.0.2/ffmpeg.zip",
        sha256: "c8ed4c4e6978a03c485edbfe4e0a5dc2380f8a30bba5150531b31b094492d924",
        bytes: 28_395_699,
        archive: Archive::Zip,
    },
    Pin {
        os: "macos",
        arch: "x86_64",
        url: "https://ffmpeg.martin-riedl.de/download/macos/amd64/1789931006_9.0.2/ffmpeg.zip",
        sha256: "7c6b4125b191cbf773832dc51f424cf2b6bb7da43007d1e066f95909e47cacd4",
        bytes: 33_816_391,
        archive: Archive::Zip,
    },
];

/// The build for this computer, if there is one.
pub(crate) fn pin_for(os: &str, arch: &str) -> Option<&'static Pin> {
    PINS.iter().find(|p| p.os == os && p.arch == arch)
}

pub(crate) fn this_pin() -> Option<&'static Pin> {
    pin_for(std::env::consts::OS, std::env::consts::ARCH)
}

/// The loader a pinned build starts through, where it needs one: the Linux builds link glibc
/// dynamically. Without it the file unpacks and hashes fine, and then "No such file or directory"
/// when run: the `FROM scratch` Docker images (they have no libc at all), Alpine.
fn loader_of(pin: &Pin) -> Option<&'static str> {
    match (pin.os, pin.arch) {
        ("linux", "x86_64") => Some("/lib64/ld-linux-x86-64.so.2"),
        ("linux", "aarch64") => Some("/lib/ld-linux-aarch64.so.1"),
        _ => None,
    }
}

/// The pinned build can run on a system where `exists` says which files there are.
pub(crate) fn runs_with(pin: &Pin, exists: impl Fn(&Path) -> bool) -> bool {
    loader_of(pin).is_none_or(|l| exists(Path::new(l)))
}

/// The build to download for this computer: one is pinned, and it can run here.
fn usable_pin() -> Option<&'static Pin> {
    this_pin().filter(|p| runs_with(p, Path::exists))
}

fn exe_name() -> &'static str {
    if cfg!(windows) {
        "ffmpeg.exe"
    } else {
        "ffmpeg"
    }
}

/// `<data>/tools/ffmpeg-<version>`.
pub(crate) fn install_dir() -> Option<PathBuf> {
    Some(
        crate::remote::store::data_dir()?
            .join("tools")
            .join(format!("ffmpeg-{FFMPEG_VERSION}")),
    )
}

/// What `installed.json` records.
#[derive(serde::Serialize, serde::Deserialize, Clone, Debug, PartialEq)]
struct Installed {
    version: String,
    url: String,
    archive_sha256: String,
    ffmpeg_sha256: String,
}

/// How to run ffmpeg: the program and the environment it needs.
#[derive(Clone, Debug)]
pub(crate) struct FfmpegCmd {
    pub program: PathBuf,
    /// Linux: `LD_LIBRARY_PATH` for the build's shared libraries.
    pub lib_dir: Option<PathBuf>,
}

impl FfmpegCmd {
    pub(crate) fn command(&self) -> tokio::process::Command {
        let mut c = tokio::process::Command::new(&self.program);
        if let Some(l) = &self.lib_dir {
            let mut v = l.as_os_str().to_owned();
            if let Some(old) = std::env::var_os("LD_LIBRARY_PATH") {
                v.push(":");
                v.push(old);
            }
            c.env("LD_LIBRARY_PATH", v);
        }
        // The builds' OpenSSL looks for trusted roots where its build machine kept them and finds
        // none here: every rtmps:// site (YouTube) failed "certificate verify failed". The
        // system's own bundle, unless the person set one.
        if std::env::var_os("SSL_CERT_FILE").is_none() {
            if let Some(ca) = system_ca_bundle() {
                c.env("SSL_CERT_FILE", ca);
            }
        }
        #[cfg(windows)]
        {
            // No console window flashing up for the child.
            c.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
        }
        c
    }
}

/// Where systems keep their bundle of trusted root certificates (PEM): macOS, then Debian,
/// Ubuntu and Arch, Fedora and RHEL, openSUSE, Alpine. Windows builds use the system's store.
const CA_BUNDLES: &[&str] = &[
    "/etc/ssl/cert.pem",
    "/etc/ssl/certs/ca-certificates.crt",
    "/etc/pki/tls/certs/ca-bundle.crt",
    "/etc/pki/ca-trust/extracted/pem/tls-ca-bundle.pem",
    "/etc/ssl/ca-bundle.pem",
];

/// This system's trusted roots for ffmpeg's TLS (rtmps://), if it has a bundle we know of.
pub(crate) fn system_ca_bundle() -> Option<&'static std::path::Path> {
    if cfg!(windows) {
        return None;
    }
    CA_BUNDLES
        .iter()
        .map(std::path::Path::new)
        .find(|p| p.is_file())
}

/// Binaries already hashed this run: (path, length, modified) → fine.
#[allow(clippy::type_complexity)]
static CHECKED: Mutex<Option<((PathBuf, u64, std::time::SystemTime), bool)>> = Mutex::new(None);

/// The verified ffmpeg, if it is installed (or `PS5UPLOAD_FFMPEG` names one).
pub(crate) fn find() -> Option<FfmpegCmd> {
    if let Some(p) = std::env::var_os("PS5UPLOAD_FFMPEG").filter(|p| !p.is_empty()) {
        return Some(FfmpegCmd {
            program: PathBuf::from(p),
            lib_dir: None,
        });
    }
    // A copy from before the image said it can't run here (kept in a Docker volume) can't run now.
    usable_pin()?;
    find_in(&install_dir()?)
}

pub(crate) fn find_in(dir: &Path) -> Option<FfmpegCmd> {
    let program = dir.join("bin").join(exe_name());
    let rec: Installed =
        serde_json::from_slice(&std::fs::read(dir.join("installed.json")).ok()?).ok()?;
    if rec.version != FFMPEG_VERSION {
        return None;
    }
    let meta = std::fs::metadata(&program).ok()?;
    let stamp = (program.clone(), meta.len(), meta.modified().ok()?);
    // The verdict for this exact file (path, size, time) is kept, good or bad: a status poll
    // every second must not hash 80 MB (and warn) each time.
    let known = CHECKED
        .lock()
        .ok()
        .and_then(|g| g.as_ref().filter(|(s, _)| *s == stamp).map(|(_, ok)| *ok));
    let ok = match known {
        Some(ok) => ok,
        None => {
            let ok =
                sha256_file(&program).is_ok_and(|got| got.eq_ignore_ascii_case(&rec.ffmpeg_sha256));
            if !ok {
                crate::log_warn!(
                    "ffmpeg_tool: {} does not match what was installed; not running it",
                    program.display()
                );
            }
            if let Ok(mut g) = CHECKED.lock() {
                *g = Some((stamp, ok));
            }
            ok
        }
    };
    if !ok {
        return None;
    }
    let lib = dir.join("lib");
    Some(FfmpegCmd {
        program,
        lib_dir: (cfg!(target_os = "linux") && lib.is_dir()).then_some(lib),
    })
}

fn sha256_file(p: &Path) -> std::io::Result<String> {
    let mut f = std::fs::File::open(p)?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 1 << 16];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(hex(&h.finalize()))
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// The download in progress (or the last one's error), for the status.
#[derive(Clone, Debug, Default, serde::Serialize)]
pub(crate) struct InstallState {
    pub running: bool,
    /// "downloading", "verifying", "unpacking".
    pub step: Option<&'static str>,
    pub done_bytes: u64,
    pub total_bytes: u64,
    /// "unsupported", "download", "checksum", "unpack", "disk".
    pub error: Option<String>,
    pub message: Option<String>,
}

static INSTALL: Mutex<InstallState> = Mutex::new(InstallState {
    running: false,
    step: None,
    done_bytes: 0,
    total_bytes: 0,
    error: None,
    message: None,
});

fn set_state(f: impl FnOnce(&mut InstallState)) {
    if let Ok(mut g) = INSTALL.lock() {
        f(&mut g);
    }
}

pub(crate) fn install_state() -> InstallState {
    INSTALL.lock().map(|g| g.clone()).unwrap_or_default()
}

/// What the status says about ffmpeg.
pub(crate) fn status_json() -> serde_json::Value {
    let found = find();
    let pin = this_pin();
    serde_json::json!({
        "installed": found.is_some(),
        "version": FFMPEG_VERSION,
        "supported": usable_pin().is_some(),
        // There is a download for this OS and CPU, but this system can't run it (a Docker image):
        // it takes an ffmpeg of its own (`PS5UPLOAD_FFMPEG`).
        "needs_own": pin.is_some() && usable_pin().is_none(),
        "download_bytes": pin.map(|p| p.bytes),
        "source": pin.map(|p| p.url),
        "install": install_state(),
    })
}

/// Start the download (if one isn't running and ffmpeg isn't there). Ok(false): nothing to do.
pub(crate) fn start_install() -> Result<bool, (&'static str, String)> {
    if find().is_some() {
        return Ok(false);
    }
    let Some(pin) = this_pin() else {
        return Err((
            "unsupported",
            format!(
                "There is no ffmpeg download for {} {} here.",
                std::env::consts::OS,
                std::env::consts::ARCH
            ),
        ));
    };
    if !runs_with(pin, Path::exists) {
        return Err((
            "unsupported",
            "The ffmpeg download can't run on this system (a Docker image, for example): set \
             PS5UPLOAD_FFMPEG to an ffmpeg that can."
                .to_string(),
        ));
    }
    let Some(dir) = install_dir() else {
        return Err((
            "disk",
            "No folder to keep ffmpeg in; set PS5UPLOAD_DATA_DIR.".to_string(),
        ));
    };
    {
        let mut g = INSTALL.lock().map_err(|_| ("disk", "busy".to_string()))?;
        if g.running {
            return Ok(true);
        }
        *g = InstallState {
            running: true,
            step: Some("downloading"),
            total_bytes: pin.bytes,
            ..Default::default()
        };
    }
    std::thread::spawn(move || {
        let r = install(pin, &dir, &download);
        set_state(|s| {
            s.running = false;
            s.step = None;
            if let Err((code, msg)) = r {
                crate::log_warn!("ffmpeg_tool: install failed ({code}): {msg}");
                s.error = Some(code.to_string());
                s.message = Some(msg);
            } else {
                crate::log_info!("ffmpeg_tool: ffmpeg {FFMPEG_VERSION} installed");
            }
        });
    });
    Ok(true)
}

/// Fetches `url` into `w`, calling `progress(done)` as it goes.
pub(crate) type Fetch = dyn Fn(&str, &mut dyn Write, &dyn Fn(u64)) -> Result<(), String>;

fn download(url: &str, w: &mut dyn Write, progress: &dyn Fn(u64)) -> Result<(), String> {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_connect(Some(Duration::from_secs(20)))
        // Per read, not for the whole body (`timeout_recv_body` is a total: 60 s would fail a
        // 90 MB download on anything under about 12 Mbps).
        .timeout_per_read(Some(Duration::from_secs(60)))
        .http_status_as_error(false)
        .build()
        .into();
    let resp = agent.get(url).call().map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        return Err(format!("the server answered {}", resp.status()));
    }
    let mut r = resp.into_body().into_reader();
    let mut buf = vec![0u8; 1 << 16];
    let mut done = 0u64;
    loop {
        let n = r.read(&mut buf).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        w.write_all(&buf[..n]).map_err(|e| e.to_string())?;
        done += n as u64;
        progress(done);
    }
    Ok(())
}

/// A writer that hashes what goes through it.
struct HashWriter<W: Write> {
    inner: W,
    hash: Sha256,
    n: u64,
}

impl<W: Write> Write for HashWriter<W> {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        let n = self.inner.write(b)?;
        self.hash.update(&b[..n]);
        self.n += n as u64;
        Ok(n)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

/// Download, verify and unpack `pin` into `dir`. Nothing lands in `dir` unless the archive's
/// hash matched.
pub(crate) fn install(pin: &Pin, dir: &Path, fetch: &Fetch) -> Result<(), (&'static str, String)> {
    let parent = dir.parent().ok_or(("disk", "no folder".to_string()))?;
    std::fs::create_dir_all(parent).map_err(|e| ("disk", e.to_string()))?;
    let tmp = parent.join(format!("ffmpeg-{FFMPEG_VERSION}.download"));
    let staged = parent.join(format!("ffmpeg-{FFMPEG_VERSION}.partial"));
    let file = std::fs::File::create(&tmp).map_err(|e| ("disk", e.to_string()))?;
    let mut w = HashWriter {
        inner: std::io::BufWriter::new(file),
        hash: Sha256::new(),
        n: 0,
    };
    let fetched = fetch(pin.url, &mut w, &|done| set_state(|s| s.done_bytes = done));
    let flushed = w.flush();
    if let Err(e) = fetched {
        let _ = std::fs::remove_file(&tmp);
        return Err((
            "download",
            format!("Could not download ffmpeg from {}: {e}", host_of(pin.url)),
        ));
    }
    flushed.map_err(|e| ("disk", e.to_string()))?;
    set_state(|s| s.step = Some("verifying"));
    let got = hex(&w.hash.finalize());
    if !got.eq_ignore_ascii_case(pin.sha256) || w.n != pin.bytes {
        let _ = std::fs::remove_file(&tmp);
        return Err((
            "checksum",
            format!(
                "The download did not match the expected file (SHA-256 {got}, {} bytes), so it was deleted and not used.",
                w.n
            ),
        ));
    }
    set_state(|s| s.step = Some("unpacking"));
    if staged.exists() {
        let _ = std::fs::remove_dir_all(&staged);
    }
    std::fs::create_dir_all(&staged).map_err(|e| ("disk", e.to_string()))?;
    let unpacked = match pin.archive {
        Archive::Zip => unpack_zip(&tmp, &staged),
        Archive::TarXz => unpack_tar_xz(&tmp, &staged),
    };
    let _ = std::fs::remove_file(&tmp);
    if let Err(e) = unpacked {
        let _ = std::fs::remove_dir_all(&staged);
        return Err(("unpack", e));
    }
    let program = staged.join("bin").join(exe_name());
    let ffmpeg_sha256 = sha256_file(&program).map_err(|_| {
        let _ = std::fs::remove_dir_all(&staged);
        ("unpack", "The download had no ffmpeg in it.".to_string())
    })?;
    let rec = Installed {
        version: FFMPEG_VERSION.into(),
        url: pin.url.into(),
        archive_sha256: pin.sha256.into(),
        ffmpeg_sha256,
    };
    std::fs::write(
        staged.join("installed.json"),
        serde_json::to_vec_pretty(&rec).unwrap_or_default(),
    )
    .map_err(|e| ("disk", e.to_string()))?;
    if dir.exists() {
        let _ = std::fs::remove_dir_all(dir);
    }
    std::fs::rename(&staged, dir).map_err(|e| ("disk", e.to_string()))?;
    let own = std::env::var_os("PS5UPLOAD_FFMPEG").map(PathBuf::from);
    remove_old_versions(parent, dir, own.as_deref());
    Ok(())
}

/// Every other `ffmpeg-*` folder in `tools` goes: each new pinned version would otherwise leave
/// 60 to 200 MB behind for good. Not `keep`, and not the one `own` (`PS5UPLOAD_FFMPEG`) runs from.
pub(crate) fn remove_old_versions(tools: &Path, keep: &Path, own: Option<&Path>) {
    let Ok(entries) = std::fs::read_dir(tools) else {
        return;
    };
    for e in entries.flatten() {
        let path = e.path();
        let is_ours = e.file_name().to_string_lossy().starts_with("ffmpeg-");
        // Not through a link: only real folders of ours.
        let is_dir = e.file_type().is_ok_and(|t| t.is_dir());
        if !is_ours || !is_dir || path == keep || own.is_some_and(|o| o.starts_with(&path)) {
            continue;
        }
        match std::fs::remove_dir_all(&path) {
            Ok(()) => crate::log_info!("ffmpeg_tool: removed the old {}", path.display()),
            Err(e) => crate::log_warn!("ffmpeg_tool: could not remove {}: {e}", path.display()),
        }
    }
}

fn host_of(url: &str) -> &str {
    url.split("://")
        .nth(1)
        .and_then(|r| r.split('/').next())
        .unwrap_or(url)
}

/// Where an archive entry goes under the install folder, or None to skip it. Kept: `bin/` and
/// `lib/` (under the archive's one top folder, if it has one), and a bare `ffmpeg[.exe]` at the
/// top (macOS zips) as `bin/ffmpeg`. Never ffplay/ffprobe, headers, docs, or anything that
/// would land outside the folder.
pub(crate) fn place(entry: &str) -> Option<PathBuf> {
    let p = Path::new(entry);
    let mut parts: Vec<&str> = Vec::new();
    for c in p.components() {
        match c {
            Component::Normal(s) => parts.push(s.to_str()?),
            Component::CurDir => {}
            _ => return None,
        }
    }
    if parts.len() == 1 && (parts[0] == "ffmpeg" || parts[0] == "ffmpeg.exe") {
        return Some(PathBuf::from("bin").join(parts[0]));
    }
    // Under one top folder (BtbN's ffmpeg-n9.0.2-…/bin/…) or at the top.
    let rest = match parts.iter().position(|s| *s == "bin" || *s == "lib") {
        Some(i) if i <= 1 => &parts[i..],
        _ => return None,
    };
    if rest.len() < 2 {
        return None;
    }
    let name = rest[rest.len() - 1].to_ascii_lowercase();
    if rest[0] == "bin" {
        let keep = name == "ffmpeg" || name == "ffmpeg.exe" || name.ends_with(".dll");
        if !keep || rest.len() != 2 {
            return None;
        }
    } else if !(name.contains(".so") && rest.len() == 2) {
        return None;
    }
    Some(rest.iter().collect())
}

fn unpack_zip(archive: &Path, to: &Path) -> Result<(), String> {
    let f = std::fs::File::open(archive).map_err(|e| e.to_string())?;
    let mut z = zip::ZipArchive::new(f).map_err(|e| format!("not a zip: {e}"))?;
    for i in 0..z.len() {
        let mut e = z.by_index(i).map_err(|e| e.to_string())?;
        if e.is_dir() {
            continue;
        }
        let Some(rel) = e.name().ok().and_then(|n| place(&n)) else {
            continue;
        };
        let out = to.join(rel);
        if let Some(d) = out.parent() {
            std::fs::create_dir_all(d).map_err(|e| e.to_string())?;
        }
        let mut w = std::fs::File::create(&out).map_err(|e| e.to_string())?;
        std::io::copy(&mut e, &mut w).map_err(|e| e.to_string())?;
        make_executable(&out);
    }
    Ok(())
}

fn unpack_tar_xz(archive: &Path, to: &Path) -> Result<(), String> {
    let f = std::fs::File::open(archive).map_err(|e| e.to_string())?;
    let xz = lzma_rust2::XzReader::new(std::io::BufReader::new(f), true);
    unpack_tar(xz, to)
}

/// A plain ustar/GNU tar reader: regular files and symlinks (the shared libraries'
/// `libavcodec.so.62 -> libavcodec.so.62.x.y`), GNU long names, pax headers skipped.
pub(crate) fn unpack_tar(mut r: impl Read, to: &Path) -> Result<(), String> {
    let mut long_name: Option<String> = None;
    let mut long_link: Option<String> = None;
    let mut block = [0u8; 512];
    loop {
        if read_full(&mut r, &mut block)? == 0 {
            break;
        }
        if block.iter().all(|&b| b == 0) {
            break;
        }
        let field = |a: usize, b: usize| {
            let f = &block[a..b];
            let end = f.iter().position(|&c| c == 0).unwrap_or(f.len());
            String::from_utf8_lossy(&f[..end]).into_owned()
        };
        let size = parse_octal(&block[124..136]).ok_or("bad tar size")?;
        let kind = block[156];
        let mut name = field(0, 100);
        let prefix = field(345, 500);
        if &block[257..262] == b"ustar" && !prefix.is_empty() && kind != b'L' {
            name = format!("{prefix}/{name}");
        }
        let link = field(157, 257);
        let padded = size.div_ceil(512) * 512;
        match kind {
            b'L' | b'K' => {
                let mut v = vec![0u8; padded as usize];
                read_exact(&mut r, &mut v)?;
                v.truncate(size as usize);
                let s = String::from_utf8_lossy(&v)
                    .trim_end_matches('\0')
                    .to_string();
                if kind == b'L' {
                    long_name = Some(s);
                } else {
                    long_link = Some(s);
                }
                continue;
            }
            _ => {}
        }
        let name = long_name.take().unwrap_or(name);
        let link = long_link.take().unwrap_or(link);
        let target = place(&name).map(|rel| to.join(rel));
        match (kind, target) {
            (b'0' | 0, Some(out)) => {
                if let Some(d) = out.parent() {
                    std::fs::create_dir_all(d).map_err(|e| e.to_string())?;
                }
                let mut w = std::fs::File::create(&out).map_err(|e| e.to_string())?;
                let mut left = size;
                let mut buf = vec![0u8; 1 << 16];
                while left > 0 {
                    let n = (left as usize).min(buf.len());
                    read_exact(&mut r, &mut buf[..n])?;
                    w.write_all(&buf[..n]).map_err(|e| e.to_string())?;
                    left -= n as u64;
                }
                skip(&mut r, padded - size)?;
                make_executable(&out);
            }
            (b'2', Some(out)) => {
                skip(&mut r, padded)?;
                // Only a link to a file beside it (no folders, nothing outside).
                if link.contains('/') || link.contains('\\') || link.is_empty() {
                    continue;
                }
                if let Some(d) = out.parent() {
                    std::fs::create_dir_all(d).map_err(|e| e.to_string())?;
                }
                symlink(&link, &out)?;
            }
            _ => skip(&mut r, padded)?,
        }
    }
    Ok(())
}

#[cfg(unix)]
fn symlink(link: &str, out: &Path) -> Result<(), String> {
    let _ = std::fs::remove_file(out);
    std::os::unix::fs::symlink(link, out).map_err(|e| e.to_string())
}

#[cfg(not(unix))]
fn symlink(link: &str, out: &Path) -> Result<(), String> {
    // A Linux build is never unpacked here; copying keeps the reader total anyway.
    let src = out.with_file_name(link);
    std::fs::copy(src, out)
        .map(|_| ())
        .map_err(|e| e.to_string())
}

fn parse_octal(f: &[u8]) -> Option<u64> {
    if f[0] & 0x80 != 0 {
        // GNU base-256 size.
        let mut v = 0u64;
        for &b in &f[1..] {
            v = v.checked_mul(256)?.checked_add(b as u64)?;
        }
        return Some(v);
    }
    let s: String = f
        .iter()
        .take_while(|&&c| c != 0 && c != b' ')
        .map(|&c| c as char)
        .collect();
    let s = s.trim();
    if s.is_empty() {
        return Some(0);
    }
    u64::from_str_radix(s, 8).ok()
}

fn read_full(r: &mut impl Read, buf: &mut [u8]) -> Result<usize, String> {
    let mut n = 0;
    while n < buf.len() {
        match r.read(&mut buf[n..]) {
            Ok(0) => break,
            Ok(k) => n += k,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e.to_string()),
        }
    }
    if n != 0 && n != buf.len() {
        return Err("the archive ends early".into());
    }
    Ok(n)
}

fn read_exact(r: &mut impl Read, buf: &mut [u8]) -> Result<(), String> {
    r.read_exact(buf)
        .map_err(|e| format!("the archive ends early: {e}"))
}

fn skip(r: &mut impl Read, n: u64) -> Result<(), String> {
    let copied = std::io::copy(&mut r.take(n), &mut std::io::sink()).map_err(|e| e.to_string())?;
    if copied != n {
        return Err("the archive ends early".into());
    }
    Ok(())
}

#[cfg(unix)]
fn make_executable(p: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o755));
}

#[cfg(not(unix))]
fn make_executable(_: &Path) {}

#[cfg(test)]
#[path = "ffmpeg_tool_tests.rs"]
mod tests;
