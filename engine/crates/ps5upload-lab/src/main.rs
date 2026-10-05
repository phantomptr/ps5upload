//! ps5upload-lab — CLI tool for exercising the console's management and data channels.
//!
//! Usage:
//!   ps5upload-lab [ADDR] COMMAND [ARGS...]
//!
//! ADDR is the console's host (default 192.168.137.2). A trailing `:port` is accepted and
//! ignored: every command goes over AVA1 (data and management share one port, 9120).
//!
//! Commands: see `ps5upload-lab` with no arguments.

use anyhow::{bail, Context, Result};
use ps5upload_core::diagnostics::shell_run;
use ps5upload_core::fs_ops::{app_launch, app_list_registered, app_register, app_unregister};
use ps5upload_core::hw::{hw_info, hw_temps, syslog_tail};
use ps5upload_core::payload_lifecycle::{send_elf_to_loader, LoaderImage};
use ps5upload_core::saves::list_saves;
use ps5upload_core::transfer::TransferConfig;
use ps5upload_core::volumes::list_volumes;
use std::path::Path;

mod bench;

const DEFAULT_ADDR: &str = "192.168.137.2";

// ─── Helpers ─────────────────────────────────────────────────────────────────

/// The console's host from whatever the caller typed: `host`, `host:9113` and `host:9114` all
/// mean the same console (the ports they name no longer exist; AVA1 has one). A bracketed IPv6
/// literal keeps its brackets; a bare one (several colons) has no port to lose.
fn console_host(addr: &str) -> String {
    if let Some(rest) = addr.strip_prefix('[') {
        return match rest.find(']') {
            Some(i) => format!("[{}]", &rest[..i]),
            None => addr.to_string(),
        };
    }
    match addr.split_once(':') {
        Some((host, port)) if !port.contains(':') => host.to_string(),
        _ => addr.to_string(),
    }
}

fn parse_tx_id(hex: &str) -> Result<[u8; 16]> {
    if hex.len() != 32 {
        bail!("tx_id must be exactly 32 hex chars, got {}", hex.len());
    }
    let mut out = [0u8; 16];
    for (i, chunk) in hex.as_bytes().chunks(2).enumerate() {
        let hi = hex_val(chunk[0])?;
        let lo = hex_val(chunk[1])?;
        out[i] = (hi << 4) | lo;
    }
    Ok(out)
}

fn hex_val(b: u8) -> Result<u8> {
    match b {
        b'0'..=b'9' => Ok(b - b'0'),
        b'a'..=b'f' => Ok(10 + b - b'a'),
        b'A'..=b'F' => Ok(10 + b - b'A'),
        _ => bail!("invalid hex char: {}", b as char),
    }
}

// ─── Commands ────────────────────────────────────────────────────────────────

fn do_volumes(addr: &str) -> Result<()> {
    let vols = list_volumes(addr)?;
    if vols.volumes.is_empty() {
        println!("(no volumes detected)");
        return Ok(());
    }
    // Render a small human-readable table. Fixed-width columns so the
    // common case (3-5 volumes, short mount names) is easy to eyeball.
    println!(
        "{:<8}  {:<8}  {:>14}  {:>14}  RW",
        "PATH", "FS", "TOTAL", "FREE"
    );
    for v in &vols.volumes {
        println!(
            "{:<8}  {:<8}  {:>14}  {:>14}  {}",
            v.path,
            v.fs_type,
            format_bytes(v.total_bytes),
            format_bytes(v.free_bytes),
            if v.writable { "rw" } else { "ro" }
        );
    }
    Ok(())
}

fn format_bytes(b: u64) -> String {
    const UNITS: &[&str] = &["B", "KiB", "MiB", "GiB", "TiB"];
    let mut v = b as f64;
    let mut i = 0;
    while v >= 1024.0 && i < UNITS.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    format!("{:.2} {}", v, UNITS[i])
}

/// `node.info` over an AVA1 session.
fn do_hello(addr: &str) -> Result<()> {
    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(ava1_cmds::hello(addr))
}

/// What an upload command prints: the result's counters and the job's final status JSON.
fn print_upload(r: &ps5upload_core::transfer::TransferResult) {
    println!(
        "done: files={} bytes={} job={}",
        r.files_sent, r.bytes_sent, r.tx_id_hex
    );
    println!("final status: {}", r.commit_ack_body);
}

fn do_transfer(addr: &str, id_hex: &str, dest: &str, file_path: &str) -> Result<()> {
    let id = parse_tx_id(id_hex)?;
    let cfg = TransferConfig::new(addr);
    println!("upload: file={file_path} dest={dest}");
    print_upload(&ps5upload_ava1::upload::upload_file(
        &cfg,
        id,
        dest,
        Path::new(file_path),
    )?);
    Ok(())
}

fn do_transfer_dir(addr: &str, id_hex: &str, dest_root: &str, src_dir: &str) -> Result<()> {
    let id = parse_tx_id(id_hex)?;
    let cfg = TransferConfig::new(addr);
    print_upload(&ps5upload_ava1::upload::upload_dir(
        &cfg,
        id,
        dest_root,
        Path::new(src_dir),
    )?);
    Ok(())
}

fn do_transfer_zip(addr: &str, id_hex: &str, dest_root: &str, zip_path: &str) -> Result<()> {
    let id = parse_tx_id(id_hex)?;
    let cfg = TransferConfig::new(addr);
    let zp = Path::new(zip_path);
    let ins = ps5upload_core::transfer::inspect_zip(zp)?;
    println!(
        "zip: {} files, {} zipped -> {} extracted",
        ins.file_count, ins.compressed_size, ins.total_uncompressed
    );
    print_upload(&ps5upload_ava1::upload::upload_zip(
        &cfg, id, dest_root, zp,
    )?);
    Ok(())
}

fn do_transfer_7z(addr: &str, id_hex: &str, dest_root: &str, archive_path: &str) -> Result<()> {
    let id = parse_tx_id(id_hex)?;
    let cfg = TransferConfig::new(addr);
    let ap = Path::new(archive_path);
    let ins = ps5upload_core::transfer::inspect_7z(ap)?;
    println!(
        "7z: {} files, {} compressed -> {} extracted",
        ins.file_count, ins.compressed_size, ins.total_uncompressed
    );
    print_upload(&ps5upload_ava1::upload::upload_7z(&cfg, id, dest_root, ap)?);
    Ok(())
}

fn do_transfer_rar(
    addr: &str,
    id_hex: &str,
    dest_root: &str,
    archive_path: &str,
    password: Option<&str>,
) -> Result<()> {
    let id = parse_tx_id(id_hex)?;
    let cfg = TransferConfig::new(addr);
    let ap = Path::new(archive_path);
    let ins = ps5upload_core::transfer::inspect_rar(ap, password)?;
    println!(
        "rar: {} files, {} extracted",
        ins.file_count, ins.total_uncompressed
    );
    print_upload(&ps5upload_ava1::upload::upload_rar(
        &cfg, id, dest_root, ap, password,
    )?);
    Ok(())
}

/// `node.status`: the payload's status JSON (version, kernel, fans, ...).
fn do_status(addr: &str) -> Result<()> {
    let body = ps5upload_core::mgmt::call(addr, ps5upload_core::mgmt::m::NODE_STATUS, b"")?;
    println!("{}", String::from_utf8_lossy(&body));
    Ok(())
}

fn do_hw_info(addr: &str) -> Result<()> {
    let info = hw_info(addr)?;
    println!("{info:?}");
    Ok(())
}

fn do_hw_temps(addr: &str, extended: bool) -> Result<()> {
    let t = hw_temps(addr, extended)?;
    println!("{t:?}");
    Ok(())
}

fn do_shutdown(addr: &str) -> Result<()> {
    let body = ps5upload_core::mgmt::call(addr, ps5upload_core::mgmt::m::NODE_SHUTDOWN, b"")?;
    println!("{}", String::from_utf8_lossy(&body));
    Ok(())
}

/// Streams a local ELF to the console's loader on :9021 — the first step of a hardware
/// pass, before any `ava1-*` command. `send_elf_to_loader` shuts the running payload down
/// over management and waits the same 600 ms grace the desktop send uses.
fn do_send_elf(addr: &str, file: &str) -> Result<()> {
    let host = addr;
    let bytes = std::fs::read(file).with_context(|| format!("read {file}"))?;
    let n = send_elf_to_loader(host, 9021, &bytes, LoaderImage::Ps5Upload)
        .map_err(|e| anyhow::anyhow!(e))?;
    println!("sent {n} bytes to {host}:9021");
    Ok(())
}

fn do_saves(addr: &str) -> Result<()> {
    let list = list_saves(addr, 0)?;
    for s in &list.saves {
        println!(
            "{:<16} size={:<12} kind={:<3} path={}",
            s.title_id, s.size, s.kind, s.path
        );
    }
    println!("({} save(s))", list.saves.len());
    Ok(())
}

fn do_profile_info(addr: &str) -> Result<()> {
    let info = ps5upload_core::profile::profile_info(addr)?;
    println!(
        "foreground user: uid={} ({}) name={:?}",
        info.uid, info.uid_hex, info.username
    );
    println!("local users ({}):", info.users.len());
    for u in &info.users {
        println!("  {} uid={} name={:?}", u.uid_hex, u.uid, u.username);
    }
    if info.slots.is_empty() {
        println!("(no offline-account name slots populated)");
    }
    for s in &info.slots {
        println!(
            "  slot {:>2}: name={:?} type={:?} flags={} id={} activated={}",
            s.slot, s.name, s.type_, s.flags, s.id, s.activated
        );
    }
    Ok(())
}

fn do_profile_set_username(addr: &str, slot: i32, name: &str) -> Result<()> {
    ps5upload_core::profile::profile_set_username(addr, slot, name)?;
    println!("renamed slot {slot} -> {name:?}");
    Ok(())
}

fn do_profile_rename_user(addr: &str, uid: u32, name: &str) -> Result<()> {
    ps5upload_core::profile::profile_set_local_username(addr, uid, name)?;
    println!("renamed user 0x{uid:08X} -> {name:?}");
    Ok(())
}

fn do_profile_activate(addr: &str, slot: i32, id: Option<u64>) -> Result<()> {
    let id = ps5upload_core::profile::profile_activate(addr, slot, id)?;
    println!("activated slot {slot}, id={id}");
    Ok(())
}

fn do_profile_clear_slot(addr: &str, slot: i32) -> Result<()> {
    ps5upload_core::profile::profile_clear_slot(addr, slot)?;
    println!("cleared slot {slot}");
    Ok(())
}

fn do_profile_apply_avatar(
    addr: &str,
    image_path: &str,
    mode: &str,
    uid: Option<u32>,
) -> Result<()> {
    let bytes = std::fs::read(image_path)?;
    let mode = ps5upload_core::profile::SquareMode::parse(mode);
    let applied =
        ps5upload_core::profile::profile_apply_avatar(addr, uid.unwrap_or(0), None, &bytes, mode)?;
    println!(
        "avatar applied: uid={} username={:?} files_copied={}",
        applied.uid, applied.username, applied.files_copied
    );
    Ok(())
}

// ─── Entry point ─────────────────────────────────────────────────────────────

mod ava1_cmds {
    use std::io::{BufRead, Write};
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use anyhow::{anyhow, bail, Context, Result};
    use ava1::keys::Identity;
    use ava1::launch::LaunchTokens;
    use ava1::peers::PeerStore;
    use ava1::session::{connect, Session, Timing};

    /// The lab's data dir — same order as the engine's data_dir(): HOME, then
    /// USERPROFILE, overridden by PS5UPLOAD_DATA_DIR.
    pub fn data_dir() -> PathBuf {
        std::env::var("PS5UPLOAD_DATA_DIR")
            .ok()
            .filter(|v| !v.trim().is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                let home = std::env::var("HOME")
                    .or_else(|_| std::env::var("USERPROFILE"))
                    .unwrap_or_else(|_| ".".into());
                PathBuf::from(home).join(".ps5upload")
            })
    }

    /// Same files the engine uses (`<data dir>/ava/`), so a lab-stamped payload trusts the engine.
    pub(crate) fn ava_dir() -> PathBuf {
        if let Ok(p) = std::env::var("AVA1_DIR") {
            return PathBuf::from(p);
        }
        data_dir().join("ava")
    }

    /// C2/A3: the default results file is `<data dir>/bench-results.jsonl` — the same
    /// directory the engine treats as its data dir, never the repo or an implicit CWD.
    /// Commands that record take `--out PATH` to override it.
    // Consumed by the follow-up calibrate arm (Task 26b) and Task 27's runner.
    pub fn bench_results_default() -> PathBuf {
        data_dir().join("bench-results.jsonl")
    }

    pub(crate) fn identity() -> Result<Arc<Identity>> {
        let p = ava_dir().join("identity");
        Ok(Arc::new(
            Identity::load_or_create(&p).with_context(|| format!("identity {}", p.display()))?,
        ))
    }

    /// `192.168.1.5` or `192.168.1.5:<any port>` → `192.168.1.5:9120` (`AVA1_PORT` overrides the
    /// port, e.g. to go through a local chaos proxy).
    pub fn ava1_addr(addr: &str) -> String {
        let host = addr.rsplit_once(':').map(|(h, _)| h).unwrap_or(addr);
        let port = std::env::var("AVA1_PORT")
            .ok()
            .and_then(|p| p.parse::<u16>().ok())
            .unwrap_or(ava1::gen::DEFAULT_PORT);
        format!("{host}:{port}")
    }

    async fn session(addr: &str) -> Result<Session> {
        // The launch tokens this lab stamped (ava1-stamp): a helper it launched proves
        // one and is trusted without a pairing code (SPEC.md §5.2).
        let peers = Arc::new(Mutex::new(
            PeerStore::load(&ava_dir().join("peers"))?
                .with_launch_tokens(LaunchTokens::at(&ava_dir().join("launch_tokens"))),
        ));
        let mut s = connect(
            &ava1_addr(addr),
            identity()?,
            peers,
            "ps5upload-lab",
            Timing::default(),
        )
        .await?;
        if s.pairing_pending() {
            print!(
                "Pairing with {}. Enter the 6-digit code shown on the console: ",
                s.peer_name()
            );
            std::io::stdout().flush()?;
            let mut line = String::new();
            std::io::stdin().lock().read_line(&mut line)?;
            let Ok(typed) = line.trim().parse::<u32>() else {
                bail!("not a code");
            };
            s.confirm_pairing(typed).await?;
            println!("paired");
        }
        Ok(s)
    }

    /// `node.info`: who the console says it is.
    pub async fn hello(addr: &str) -> Result<()> {
        let s = session(addr).await?;
        let info = s.node_info().await?;
        println!(
            "node.info: {} {} {} fw={}",
            info.name,
            info.platform,
            info.version,
            info.firmware.unwrap_or_default()
        );
        s.close().await;
        Ok(())
    }

    pub async fn ping(addr: &str, seconds: u64) -> Result<()> {
        let s = session(addr).await?;
        let info = s.node_info().await?;
        println!(
            "node.info: {} {} {} fw={}",
            info.name,
            info.platform,
            info.version,
            info.firmware.unwrap_or_default()
        );
        for i in 0..seconds {
            tokio::time::sleep(Duration::from_secs(1)).await;
            if s.is_closed() {
                bail!("session ended after {i} s: {}", s.closed().await);
            }
            println!("{:>4} s  rtt {:?}", i + 1, s.rtt());
        }
        s.close().await;
        println!("ok");
        Ok(())
    }

    pub async fn lanes(addr: &str, n: usize, seconds: u64) -> Result<()> {
        let s = session(addr).await?;
        let mut lanes = Vec::new();
        for _ in 0..n {
            lanes.push(s.open_lane().await?);
        }
        println!("{} lanes open", lanes.len());
        for i in 0..seconds {
            tokio::time::sleep(Duration::from_secs(1)).await;
            let dead = lanes.iter().filter(|l| l.is_closed()).count();
            if dead > 0 || s.is_closed() {
                bail!(
                    "after {i} s: {dead} lanes closed, session closed: {}",
                    s.is_closed()
                );
            }
        }
        println!("ok");
        Ok(())
    }

    /// Asks the console to accept new pairings for `seconds` (this machine must be paired).
    pub async fn pairing_open(addr: &str, seconds: u16) -> Result<()> {
        let s = session(addr).await?;
        s.open_pairing(seconds).await?;
        println!("pairing open on {} for {seconds} s", s.peer_name());
        s.close().await;
        Ok(())
    }

    /// Frame AEAD (ChaCha20-Poly1305) cost on the console (1 MiB frames sealed and opened
    /// in its memory with the code its transfers use) and on this computer.
    pub async fn cryptobench(addr: &str, mib: u16) -> Result<()> {
        use ava1::wire::Message;
        let s = session(addr).await?;
        let body = ava1::gen::CryptoBench { mib }.to_bytes()?;
        let r = s.rpc(ava1::gen::METHOD_CRYPTO_BENCH, &body).await?;
        if r.status != ava1::gen::STATUS_OK {
            bail!("crypto.bench failed with status {}", r.status);
        }
        let res = ava1::gen::CryptoBenchResult::decode(&r.body)?;
        let rate = |micros: u64| res.bytes as f64 / micros.max(1) as f64; // bytes per µs = MB/s
        let seal = rate(res.micros);
        println!(
            "console: seal {seal:.0} MB/s, open {} on one core ({} MiB, ChaCha20 path: {})",
            res.open_micros
                .map_or("n/a (older helper)".into(), |m| format!(
                    "{:.0} MB/s",
                    rate(m)
                )),
            res.bytes >> 20,
            res.backend
                .as_deref()
                .unwrap_or("unreported (older helper)")
        );
        let (here_seal, here_open) = local_aead_rate(mib.max(1));
        println!("this computer: seal {here_seal:.0} MB/s, open {here_open:.0} MB/s on one core");
        let worst = res.open_micros.map_or(seal, |m| seal.min(rate(m)));
        println!(
            "110 MB/s of transfer costs {:.1}% of one console core (target: at most 15%)",
            110.0 / worst * 100.0
        );
        s.close().await;
        Ok(())
    }

    /// MB/s of `ava1::keys::seal` and `open` here, on `mib` 1 MiB frames.
    fn local_aead_rate(mib: u16) -> (f64, f64) {
        const MIB: usize = 1 << 20;
        let key = [0x11u8; 32];
        let mut buf = vec![0x5au8; MIB];
        let t = std::time::Instant::now();
        for i in 0..u64::from(mib) {
            ava1::keys::seal(&key, i, &[], &mut buf);
            buf.truncate(MIB);
        }
        let seal = t.elapsed();
        let mut sealed = vec![0x5au8; MIB];
        ava1::keys::seal(&key, 0, &[], &mut sealed);
        // Opening needs the same frame every round: copy it in, and take the copies' time out.
        let t = std::time::Instant::now();
        for _ in 0..mib {
            buf.clear();
            buf.extend_from_slice(std::hint::black_box(&sealed));
        }
        let copy = t.elapsed();
        let t = std::time::Instant::now();
        for _ in 0..mib {
            buf.clear();
            buf.extend_from_slice(&sealed);
            assert!(
                ava1::keys::open(&key, 0, &[], &mut buf),
                "local open failed"
            );
        }
        let open = t.elapsed().saturating_sub(copy);
        let rate = |d: std::time::Duration| {
            f64::from(u32::from(mib)) * MIB as f64 / d.as_micros().max(1) as f64
        };
        (rate(seal), rate(open))
    }

    pub fn stamp(input: &str, output: &str) -> Result<()> {
        let mut b = std::fs::read(input).with_context(|| format!("read {input}"))?;
        let key = identity()?.public();
        // A fresh launch token with every stamp (SPEC.md §5.2): the helper we are about
        // to send proves it holds this one, so it needs no pairing code. A token that
        // cannot be kept is not stamped — the key alone still works, with a code.
        let tokens = LaunchTokens::at(&ava_dir().join("launch_tokens"));
        let mut failure = None;
        let outcome = ava1::trust::stamp_helper(&mut b, Some(&key), || match tokens.issue() {
            Ok(t) => Some(t),
            Err(e) => {
                failure = Some(e.to_string());
                None
            }
        });
        outcome.map_err(|e| anyhow!("{input}: {e}"))?;
        std::fs::write(output, &b).with_context(|| format!("write {output}"))?;
        let proof = match failure {
            None => " and a launch token".to_string(),
            Some(e) => format!(" (no launch token: {e} — the console will show a pairing code)"),
        };
        println!("stamped {output} with {}{proof}", ava1::hex::encode(&key));
        Ok(())
    }

    pub async fn chaos(listen_port: u16, upstream: &str, args: &[String]) -> Result<()> {
        let mut cfg = ava1_chaos::ChaosConfig::default();
        let mut it = args.iter();
        while let Some(a) = it.next() {
            let v: u64 = it
                .next()
                .ok_or_else(|| anyhow!("{a} needs a value"))?
                .parse()?;
            match a.as_str() {
                "--delay-ms" => cfg.delay = Duration::from_millis(v),
                "--kbps" => cfg.bytes_per_sec = Some(v * 1024),
                "--kill-every-s" => cfg.kill_every = Some(Duration::from_secs(v)),
                other => bail!("unknown option {other}"),
            }
        }
        let up = tokio::net::lookup_host(upstream)
            .await?
            .next()
            .ok_or_else(|| anyhow!("resolve {upstream}"))?;
        let p =
            ava1_chaos::ChaosProxy::start_on(&format!("0.0.0.0:{listen_port}"), up, cfg).await?;
        println!(
            "chaos proxy {} -> {up}. Enter: b = toggle blackhole, k = kill all, q = quit",
            p.addr
        );
        let mut on = false;
        for line in std::io::stdin().lock().lines() {
            match line?.trim() {
                "b" => {
                    on = !on;
                    p.blackhole(on);
                    println!("blackhole {on}");
                }
                "k" => {
                    p.kill_all();
                    println!("killed");
                }
                "q" => break,
                _ => {}
            }
        }
        Ok(())
    }
}

// ─── Benchmarks (Tasks 26–28) ─────────────────────────────────────────────────

/// `bench-corpus DIR large GIB | tiny N | ppsa01342 [--scale F] [--from-listing FILE] [--force]`
///
/// A2: refuses a non-empty target directory unless `--force` (which logs what is
/// being overwritten). Never deletes the target directory itself.
fn do_bench_corpus(args: &[String]) -> Result<()> {
    let dir = args.first().map(|s| s.as_str()).unwrap_or_else(|| usage());
    let mode = args.get(1).map(|s| s.as_str()).unwrap_or_else(|| usage());
    let mut force = false;
    let mut scale = 1.0f64;
    let mut scale_set = false;
    let mut listing: Option<String> = None;
    let mut positional: Vec<&String> = Vec::new();
    let mut it = args[2..].iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--force" => force = true,
            "--scale" => {
                scale = it
                    .next()
                    .ok_or_else(|| anyhow::anyhow!("--scale needs a value"))?
                    .parse()
                    .context("--scale")?;
                scale_set = true;
            }
            "--from-listing" => {
                listing = Some(
                    it.next()
                        .ok_or_else(|| anyhow::anyhow!("--from-listing needs a file"))?
                        .clone(),
                )
            }
            _ => positional.push(a),
        }
    }
    if mode != "ppsa01342" && (scale_set || listing.is_some()) {
        bail!("--scale and --from-listing only apply to ppsa01342");
    }
    let dir = Path::new(dir);
    bench::ensure_writable_target(dir, force)?;
    match mode {
        "large" => {
            let gib: u64 = positional
                .first()
                .and_then(|s| s.parse().ok())
                .unwrap_or_else(|| usage());
            bench::corpus_large(dir, gib)?;
            println!(
                "wrote {}/large-{gib}g.bin ({} B, BLAKE3 XOF, seed {gib})",
                dir.display(),
                gib << 30
            );
        }
        "tiny" => {
            let n: u64 = positional
                .first()
                .and_then(|s| s.parse().ok())
                .unwrap_or_else(|| usage());
            bench::corpus_tiny(dir, n)?;
            println!(
                "wrote {n} files (1–64 KiB, 64 per directory) into {}",
                dir.display()
            );
        }
        "ppsa01342" => match listing {
            Some(f) => {
                bench::corpus_listing(dir, Path::new(&f), scale)?;
                println!(
                    "reproduced the listing {f} into {} (scale {scale}; the duplicate ratio \
                     will report undefined — a listing carries no real bytes)",
                    dir.display()
                );
            }
            None => {
                bench::corpus_ppsa01342(dir, scale, bench::PPSA_COUNT)?;
                println!(
                    "wrote {} files into {} (scale {scale})",
                    bench::PPSA_COUNT,
                    dir.display()
                );
            }
        },
        other => bail!("unknown corpus mode: {other} (large | tiny | ppsa01342)"),
    }
    Ok(())
}

/// `bench-stats DIR` — files, bytes, size histogram, compressible fraction and
/// duplicate ratio. Single-threaded by design (C13): a 223 000-file corpus reads
/// ~15 GiB of samples and deflates them, so expect minutes, not seconds.
fn do_bench_stats(args: &[String]) -> Result<()> {
    let dir = args.first().map(|s| s.as_str()).unwrap_or_else(|| usage());
    let st = bench::stats(Path::new(dir))?;
    println!("files: {}", st.files);
    println!("bytes: {} ({})", st.bytes, format_bytes(st.bytes));
    println!("size histogram:");
    for (label, n) in bench::histogram_labels().iter().zip(st.histogram.iter()) {
        println!("  {label:<12} {n}");
    }
    println!(
        "compressible_fraction: {:.4}  (byte-weighted share of sampled bytes whose \
         deflate output is ≤ 90 %; sample = head+middle+tail 3×340 KiB for files > 1 MiB, \
         the whole file otherwise)",
        st.compressible_fraction
    );
    if st.duplicate_ratio.is_nan() {
        println!(
            "duplicate_ratio: undefined  (corpus reproduced from a listing — the listing \
             carries no real bytes, so a duplicate-by-content ratio is not measurable; \
             reported as undefined, never 0)"
        );
    } else {
        println!(
            "duplicate_ratio: {:.4}  (files < 64 KiB whose whole-file BLAKE3 repeats)",
            st.duplicate_ratio
        );
    }
    Ok(())
}

fn calibration_record(
    console: &str,
    dir: &str,
    files: u32,
    size: u32,
    points: &[ava1::gen::CalPoint],
) -> serde_json::Value {
    let mut record = bench::record_envelope("calibrate", None, None, "ava1");
    let object = record
        .as_object_mut()
        .expect("record envelope is an object");
    object.insert("console".into(), console.into());
    object.insert("dir".into(), dir.into());
    object.insert("files".into(), files.into());
    object.insert("size_bytes".into(), size.into());
    object.insert(
        "points".into(),
        serde_json::Value::Array(
            points
                .iter()
                .map(|p| {
                    serde_json::json!({
                        "workers": p.workers,
                        "files_per_s": p.files_per_s,
                        "create_ms": p.create_us as f64 / 1000.0,
                        "fsync_ms": p.fsync_us as f64 / 1000.0,
                    })
                })
                .collect(),
        ),
    );
    record
}

async fn do_ava1_calibrate(args: &[String]) -> Result<()> {
    let console = args
        .first()
        .ok_or_else(|| anyhow::anyhow!("ava1-calibrate needs a console"))?;
    let dir = args
        .get(1)
        .ok_or_else(|| anyhow::anyhow!("ava1-calibrate needs a destination directory"))?;
    let mut files = 2000u32;
    let mut size = 4096u32;
    let mut out = ava1_cmds::bench_results_default();
    let mut positional = 0;
    let mut i = 2;
    while i < args.len() {
        if args[i] == "--out" {
            i += 1;
            out = args
                .get(i)
                .ok_or_else(|| anyhow::anyhow!("--out needs a path"))?
                .into();
        } else {
            let v: u32 = args[i]
                .parse()
                .with_context(|| format!("invalid calibration value: {}", args[i]))?;
            match positional {
                0 => files = v,
                1 => size = v,
                _ => bail!("too many calibration values"),
            }
            positional += 1;
        }
        i += 1;
    }
    let session = ps5upload_ava1::pool().session(console).await?;
    let points = session.calibrate(dir, files, size).await?;
    println!("workers  files/s  create_us  fsync_us");
    for p in &points {
        println!(
            "{:>7}  {:>7}  {:>9}  {:>8}",
            p.workers, p.files_per_s, p.create_us, p.fsync_us
        );
    }
    bench::record(
        &out,
        &calibration_record(console, dir, files, size, &points),
    )?;
    println!("recorded {}", out.display());
    Ok(())
}

fn usage() -> ! {
    eprintln!(
        "  ava1-ping [SECONDS]            AVA1 handshake (pairs if needed), node.info, heartbeats"
    );
    eprintln!("  ava1-lanes N [SECONDS]         open N data lanes and keep them up");
    eprintln!("  ava1-cryptobench [MIB]         frame AEAD cost on the console vs this computer");
    eprintln!("  ava1-pairing-open [SECONDS]    let another device pair with the console");
    eprintln!("  ava1-stamp IN.elf OUT.elf      stamp this machine's AVA1 key into a payload");
    eprintln!("  chaos-proxy PORT HOST:PORT [--delay-ms N] [--kbps N] [--kill-every-s N]");
    eprintln!("  bench-corpus DIR large GIB    one incompressible file (BLAKE3 XOF, seed = GIB)");
    eprintln!(
        "  bench-corpus DIR tiny N       N files of 1–64 KiB, 64 per directory (~2 % duplicates)"
    );
    eprintln!("  bench-corpus DIR ppsa01342 [--scale F] [--from-listing FILE] [--force]");
    eprintln!("                                synthetic PPSA01342 shape, 223 000 files ≈ 30 GB at scale 1.0");
    eprintln!(
        "  bench-corpus … [--force]      refuse a non-empty target directory without --force"
    );
    eprintln!("  bench-stats DIR               files, bytes, size histogram, compressible fraction, duplicate");
    eprintln!(
        "                                ratio (single-threaded; a 223k-file corpus takes minutes)"
    );
    eprintln!("  ava1-calibrate CONSOLE DIR [FILES=2000] [SIZE=4096] [--out FILE]");
    eprintln!("  bench CONSOLE SCENARIO --proto ava1 --src P [--dest P] [--runs N] [--elf F]");
    eprintln!("        [--to CONSOLE2] [--out FILE] [--kill-every-s N]   (see `bench --help`)");
    eprintln!(
        "  ava1-relay FROM SRC TO DEST [TX_ID_HEX]   relay a console tree through this computer"
    );
    eprintln!("                                five disk points; FILES ≤ 20000, SIZE ≤ 1 MiB");
    eprintln!("Usage: ps5upload-lab [ADDR] COMMAND [ARGS...]");
    eprintln!("  Default ADDR: {DEFAULT_ADDR} (a host; host:9113 and host:9114 are accepted and mean the same console)");
    eprintln!("Commands:");
    eprintln!("  hello                      node.info over AVA1");
    eprintln!("  status                     node.status JSON");
    eprintln!("  shutdown                   node.shutdown: stop the running payload");
    eprintln!("  send-elf FILE      stream a local ELF to the loader on :9021 (shuts the running helper down first)");
    eprintln!("  transfer     JOB_ID_HEX DEST_FILE FILE_PATH   upload one file over AVA1");
    eprintln!("  transfer-dir JOB_ID_HEX DEST_ROOT SRC_DIR     upload a folder over AVA1");
    eprintln!("  transfer-zip JOB_ID_HEX DEST_ROOT ZIP_PATH    upload a .zip's contents");
    eprintln!("  transfer-7z  JOB_ID_HEX DEST_ROOT 7Z_PATH     upload a .7z's contents");
    eprintln!("  transfer-rar JOB_ID_HEX DEST_ROOT RAR_PATH [PASSWORD]  upload a .rar's contents");
    eprintln!("  register     SRC_PATH      register a game folder");
    eprintln!("  unregister   TITLE_ID      reverse registration");
    eprintln!("  launch       TITLE_ID      sceLncUtilLaunchApp");
    eprintln!("  power        tick|standby|reboot|shutdown  (tick = keep-awake)");
    eprintln!("  apps                       list titles present in app.db");
    eprintln!(
        "  processes                  detailed process list (pid/comm/title/mem/threads/kind)"
    );
    eprintln!("  process-kill <pid>         SIGKILL a process by pid");
    eprintln!("  saves                      list save-data folders + sizes");
    eprintln!("  profile-info                       foreground user + account name slots");
    eprintln!("  profile-set-username SLOT NAME     rename an account-name slot");
    eprintln!("  profile-rename-user  UID_HEX NAME  rename a local console user");
    eprintln!("  profile-activate     SLOT [ID_HEX] activate a slot (derive id if omitted)");
    eprintln!("  profile-clear-slot   SLOT          de-activate a slot (zero id+flags)");
    eprintln!("  profile-apply-avatar IMAGE [crop|fit]  set the foreground user's avatar");
    eprintln!("  shell       SESSION CWD CMD...   run a shell command through management");
    std::process::exit(1);
}

fn do_register(addr: &str, src_path: &str) -> Result<()> {
    /* lab CLI defaults to NOT patching DRM-type — for ad-hoc
     * registration of well-formed dumps. The desktop client UI
     * exposes the toggle. */
    let res = app_register(addr, src_path, false)?;
    println!(
        "registered: title_id={} title_name={} used_nullfs={}",
        res.title_id, res.title_name, res.used_nullfs
    );
    Ok(())
}

fn do_unregister(addr: &str, title_id: &str) -> Result<()> {
    let outcome = app_unregister(addr, title_id)?;
    if outcome.sony_refused() {
        println!(
            "unregistered: {title_id} (our teardown ok, but the console's own \
             uninstaller REFUSED with rc=0x{:08X} — the title may remain in \
             Settings > Storage)",
            outcome.sony_uninstall_rc
        );
    } else {
        println!("unregistered: {title_id}");
    }
    Ok(())
}

fn do_launch(addr: &str, title_id: &str) -> Result<()> {
    app_launch(addr, title_id)?;
    println!("launched: {title_id}");
    Ok(())
}

fn do_apps(addr: &str) -> Result<()> {
    let apps = app_list_registered(addr)?;
    if apps.apps.is_empty() {
        println!("(no registered titles)");
        return Ok(());
    }
    println!("{:<12} {:<40} {:<30} IMG", "TITLE_ID", "TITLE_NAME", "SRC");
    for a in &apps.apps {
        println!(
            "{:<12} {:<40.40} {:<30.30} {}",
            a.title_id,
            a.title_name,
            if a.src.is_empty() { "-" } else { &a.src },
            if a.image_backed { "yes" } else { "no" }
        );
    }
    Ok(())
}

fn do_processes(addr: &str) -> Result<()> {
    let res = ps5upload_core::process_mgr::process_list(addr)?;
    if res.processes.is_empty() {
        println!("(no processes)");
        return Ok(());
    }
    println!(
        "{:>6} {:<6} {:<20} {:<14} {:>8} {:>4}  NAME",
        "PID", "KIND", "COMM", "TITLE_ID", "MEM_MB", "THR"
    );
    for p in &res.processes {
        println!(
            "{:>6} {:<6} {:<20.20} {:<14} {:>8.1} {:>4}  {}{}",
            p.pid,
            p.kind,
            p.comm,
            if p.title_id.is_empty() {
                "-"
            } else {
                &p.title_id
            },
            p.memory_mib,
            p.threads,
            p.name,
            if p.is_self { "  <-- SELF (helper)" } else { "" },
        );
    }
    println!(
        "\n{} process(es){}",
        res.processes.len(),
        if res.truncated {
            " (list truncated)"
        } else {
            ""
        }
    );
    Ok(())
}

fn do_process_kill(addr: &str, pid: i32) -> Result<()> {
    let ack = ps5upload_core::process_mgr::process_kill(addr, pid)?;
    println!("kill ack: ok={} pid={}", ack.ok, ack.pid);
    Ok(())
}

fn do_shell(addr: &str, session: &str, cwd: &str, cmd: &str) -> Result<()> {
    let res = shell_run(addr, cmd, Some(session), Some(cwd), 30)?;
    println!("exit_code={:?}", res.exit_code);
    println!("timed_out={}", res.timed_out);
    println!("cwd={}", res.cwd.as_deref().unwrap_or(""));
    println!("session_id={}", res.session_id.as_deref().unwrap_or(""));
    print!("{}", res.stdout);
    Ok(())
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() {
        usage();
    }

    // If first arg looks like host:port or a bare IP, use it as address.
    let (addr, rest) =
        if args[0].contains(':') || args[0].chars().next().is_some_and(|c| c.is_ascii_digit()) {
            (args[0].as_str(), &args[1..])
        } else {
            (DEFAULT_ADDR, &args[..])
        };
    // `host`, `host:9113` and `host:9114` all mean that console.
    let host = console_host(addr);
    let addr = host.as_str();

    // Every management call goes over AVA1: register the transport the engine registers.
    ps5upload_ava1::mgmt::install();

    if rest.is_empty() {
        usage();
    }

    match rest[0].as_str() {
        "ava1-ping" | "ava1-lanes" | "ava1-cryptobench" | "ava1-pairing-open" | "chaos-proxy" => {
            let rt = tokio::runtime::Runtime::new()?;
            rt.block_on(async {
                match rest[0].as_str() {
                    "ava1-cryptobench" => {
                        ava1_cmds::cryptobench(
                            addr,
                            rest.get(1).and_then(|s| s.parse().ok()).unwrap_or(256),
                        )
                        .await
                    }
                    "ava1-pairing-open" => {
                        ava1_cmds::pairing_open(
                            addr,
                            rest.get(1).and_then(|s| s.parse().ok()).unwrap_or(120),
                        )
                        .await
                    }
                    "ava1-ping" => {
                        ava1_cmds::ping(
                            addr,
                            rest.get(1).and_then(|s| s.parse().ok()).unwrap_or(10),
                        )
                        .await
                    }
                    "ava1-lanes" => {
                        let n = rest.get(1).and_then(|s| s.parse().ok()).unwrap_or(8);
                        ava1_cmds::lanes(
                            addr,
                            n,
                            rest.get(2).and_then(|s| s.parse().ok()).unwrap_or(30),
                        )
                        .await
                    }
                    _ => {
                        let port: u16 = rest
                            .get(1)
                            .and_then(|s| s.parse().ok())
                            .unwrap_or_else(|| usage());
                        let up = rest.get(2).map(|s| s.as_str()).unwrap_or_else(|| usage());
                        ava1_cmds::chaos(port, up, &rest[3..]).await
                    }
                }
            })
        }
        "ava1-stamp" => {
            let i = rest.get(1).map(|s| s.as_str()).unwrap_or_else(|| usage());
            let o = rest.get(2).map(|s| s.as_str()).unwrap_or_else(|| usage());
            ava1_cmds::stamp(i, o)
        }
        "hello" => do_hello(addr),
        "status" => do_status(addr),
        "volumes" => do_volumes(addr),
        "shutdown" => do_shutdown(addr),
        "send-elf" => {
            let file = rest.get(1).map(|s| s.as_str()).unwrap_or_else(|| usage());
            do_send_elf(addr, file)
        }
        "transfer" => {
            let id = rest.get(1).map(|s| s.as_str()).unwrap_or_else(|| usage());
            let dest = rest.get(2).map(|s| s.as_str()).unwrap_or_else(|| usage());
            let file = rest.get(3).map(|s| s.as_str()).unwrap_or_else(|| usage());
            do_transfer(addr, id, dest, file)
        }
        "transfer-dir" => {
            let id = rest.get(1).map(|s| s.as_str()).unwrap_or_else(|| usage());
            let dest = rest.get(2).map(|s| s.as_str()).unwrap_or_else(|| usage());
            let dir = rest.get(3).map(|s| s.as_str()).unwrap_or_else(|| usage());
            do_transfer_dir(addr, id, dest, dir)
        }
        "transfer-zip" => {
            let id = rest.get(1).map(|s| s.as_str()).unwrap_or_else(|| usage());
            let dest = rest.get(2).map(|s| s.as_str()).unwrap_or_else(|| usage());
            let zip = rest.get(3).map(|s| s.as_str()).unwrap_or_else(|| usage());
            do_transfer_zip(addr, id, dest, zip)
        }
        "transfer-7z" => {
            let id = rest.get(1).map(|s| s.as_str()).unwrap_or_else(|| usage());
            let dest = rest.get(2).map(|s| s.as_str()).unwrap_or_else(|| usage());
            let arc = rest.get(3).map(|s| s.as_str()).unwrap_or_else(|| usage());
            do_transfer_7z(addr, id, dest, arc)
        }
        "transfer-rar" => {
            let id = rest.get(1).map(|s| s.as_str()).unwrap_or_else(|| usage());
            let dest = rest.get(2).map(|s| s.as_str()).unwrap_or_else(|| usage());
            let arc = rest.get(3).map(|s| s.as_str()).unwrap_or_else(|| usage());
            let password = rest.get(4).map(|s| s.as_str());
            do_transfer_rar(addr, id, dest, arc, password)
        }
        "register" => {
            let src_path = rest.get(1).map(|s| s.as_str()).unwrap_or_else(|| usage());
            do_register(addr, src_path)
        }
        "unregister" => {
            let title_id = rest.get(1).map(|s| s.as_str()).unwrap_or_else(|| usage());
            do_unregister(addr, title_id)
        }
        "launch" => {
            let title_id = rest.get(1).map(|s| s.as_str()).unwrap_or_else(|| usage());
            do_launch(addr, title_id)
        }
        "apps" => do_apps(addr),
        // processes: detailed process manager enumerate (pid/comm/title/mem/
        // threads/kind). process-kill <pid>: SIGKILL one pid (guarded payload-
        // side). Hardware-verifies the in-app process manager.
        "processes" => do_processes(addr),
        "process-kill" => {
            let pid: i32 = rest
                .get(1)
                .and_then(|s| s.parse::<i32>().ok())
                .unwrap_or_else(|| usage());
            do_process_kill(addr, pid)
        }
        // hw-info: mgmt-port hardware read. hw-temps / hw-temps-x: live
        // CPU/SoC sensor read — hw-temps-x (extended) drives the ShellUI
        // ptrace path (sys_ptrace authid swap under kernel_rw_lock), the exact
        // kernel-R/W path that must serialize against installs. Used to stress
        // the kernel_rw_lock concurrently from many connections.
        "hw-info" => do_hw_info(addr),
        "syslog" => {
            print!("{}", syslog_tail(addr)?);
            Ok(())
        }
        // power <tick|standby|reboot|shutdown>: drives the SystemControl
        // mgmt frame. `tick` (sceSystemServicePowerTick) is the keep-awake
        // primitive — non-destructive, resets the console's auto-standby
        // idle timer. Added for real-hardware verification of the client's
        // keep-PS5-awake feature.
        "power" => {
            let action = match rest.get(1).map(|s| s.as_str()) {
                Some("tick") => ps5upload_core::system_control::PowerAction::Tick,
                Some("standby") => ps5upload_core::system_control::PowerAction::Standby,
                Some("reboot") => ps5upload_core::system_control::PowerAction::Reboot,
                Some("shutdown") => ps5upload_core::system_control::PowerAction::Shutdown,
                _ => usage(),
            };
            let ack = ps5upload_core::system_control::system_control(addr, action)?;
            println!(
                "power ack: ok={} action={} err={} code={}",
                ack.ok,
                ack.action.as_deref().unwrap_or("-"),
                ack.err.as_deref().unwrap_or("-"),
                ack.code.map_or("-".to_string(), |c| c.to_string()),
            );
            if !ack.ok {
                bail!("power action failed");
            }
            Ok(())
        }
        "hw-temps" => do_hw_temps(addr, false),
        "hw-temps-x" => do_hw_temps(addr, true),
        "saves" => do_saves(addr),
        "profile-info" => do_profile_info(addr),
        "profile-set-username" => {
            let slot: i32 = rest
                .get(1)
                .and_then(|s| s.parse().ok())
                .unwrap_or_else(|| usage());
            let name = rest.get(2).map(|s| s.as_str()).unwrap_or_else(|| usage());
            do_profile_set_username(addr, slot, name)
        }
        "profile-rename-user" => {
            let uid = rest.get(1).and_then(|s| {
                let s = s
                    .strip_prefix("0x")
                    .or_else(|| s.strip_prefix("0X"))
                    .unwrap_or(s);
                u32::from_str_radix(s, 16).ok()
            });
            let name = rest.get(2).map(|s| s.as_str()).unwrap_or_else(|| usage());
            match uid {
                Some(u) => do_profile_rename_user(addr, u, name),
                None => usage(),
            }
        }
        "profile-activate" => {
            let slot: i32 = rest
                .get(1)
                .and_then(|s| s.parse().ok())
                .unwrap_or_else(|| usage());
            let id = rest.get(2).and_then(|s| {
                let s = s
                    .strip_prefix("0x")
                    .or_else(|| s.strip_prefix("0X"))
                    .unwrap_or(s);
                u64::from_str_radix(s, 16).ok()
            });
            do_profile_activate(addr, slot, id)
        }
        "profile-clear-slot" => {
            let slot: i32 = rest
                .get(1)
                .and_then(|s| s.parse().ok())
                .unwrap_or_else(|| usage());
            do_profile_clear_slot(addr, slot)
        }
        "profile-apply-avatar" => {
            let image = rest.get(1).map(|s| s.as_str()).unwrap_or_else(|| usage());
            let mode = rest.get(2).map(|s| s.as_str()).unwrap_or("crop");
            let uid = rest.get(3).and_then(|s| {
                let s = s
                    .strip_prefix("0x")
                    .or_else(|| s.strip_prefix("0X"))
                    .unwrap_or(s);
                u32::from_str_radix(s, 16).ok()
            });
            do_profile_apply_avatar(addr, image, mode, uid)
        }
        "shell" => {
            let session = rest.get(1).map(|s| s.as_str()).unwrap_or_else(|| usage());
            let cwd = rest.get(2).map(|s| s.as_str()).unwrap_or_else(|| usage());
            let cmd = rest
                .get(3..)
                .filter(|v| !v.is_empty())
                .unwrap_or_else(|| usage())
                .join(" ");
            do_shell(addr, session, cwd, &cmd)
        }
        // Benchmarks (Tasks 26–28): synchronous, console-free. bench-corpus refuses
        // a non-empty target without --force (A2); bench-stats is single-threaded
        // and takes minutes on a 223k-file corpus (C13).
        "bench-corpus" => do_bench_corpus(&rest[1..]),
        "bench-stats" => do_bench_stats(&rest[1..]),
        "ava1-calibrate" => {
            let rt = tokio::runtime::Runtime::new()?;
            rt.block_on(do_ava1_calibrate(&rest[1..]))
        }
        // C2: `bench` parses its own console, never the lab's global ADDR (a bare
        // `bench` reaches here with `addr == DEFAULT_ADDR`, which it must not use).
        "bench" => {
            let tail = &rest[1..];
            if tail.iter().any(|a| a == "--help" || a == "-h") {
                println!("{}", bench::HELP);
                return Ok(());
            }
            let parsed = bench::BenchArgs::parse(tail)?;
            let rt = tokio::runtime::Runtime::new()?;
            let rows = rt.block_on(bench::run_bench(&parsed))?;
            let failed = rows.iter().filter(|r| !r.ok).count();
            if failed > 0 {
                bail!("{failed} of {} bench run(s) failed", rows.len());
            }
            Ok(())
        }
        "ava1-relay" => {
            if rest.len() < 5 || rest.len() > 6 {
                bail!("ava1-relay needs FROM SRC TO DEST [TX_ID_HEX]");
            }
            let id = match rest.get(5) {
                Some(hex) => parse_tx_id(hex)?,
                None => *uuid::Uuid::new_v4().as_bytes(),
            };
            let progress = std::sync::Arc::new(ava1::send::Progress::default());
            let report = ps5upload_ava1::relay::ps5_to_ps5(
                &rest[1],
                &rest[2],
                &rest[3],
                &rest[4],
                id,
                progress,
                std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
            )?;
            println!(
                "job={} files={} bytes={} resent={} lanes={}",
                ava1::hex::encode(&id),
                report.files,
                report.bytes,
                report.resent,
                report.max_lanes
            );
            Ok(())
        }
        cmd => bail!("unknown command: {cmd}"),
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn calibration_record_keeps_the_frozen_units_and_identity_fields() {
        let point = ava1::gen::CalPoint {
            workers: 4,
            files_per_s: 123,
            create_us: 2500,
            fsync_us: 3750,
        };
        let row = super::calibration_record("192.168.86.100", "/data/cal", 2000, 4096, &[point]);
        assert_eq!(row["schema"], 1);
        assert_eq!(row["kind"], "calibrate");
        assert_eq!(row["protocol"], "ava1");
        assert_eq!(row["console"], "192.168.86.100");
        assert_eq!(row["dir"], "/data/cal");
        assert_eq!(row["files"], 2000);
        assert_eq!(row["size_bytes"], 4096);
        assert_eq!(row["points"][0]["files_per_s"], 123);
        assert_eq!(row["points"][0]["create_ms"], 2.5);
        assert_eq!(row["points"][0]["fsync_ms"], 3.75);
        assert!(row["machine"].is_string());
        assert!(row["started_at"].is_number());
    }

    #[test]
    fn ava1_commands_use_port_9120() {
        assert_eq!(
            super::ava1_cmds::ava1_addr("192.168.86.100:9113"),
            "192.168.86.100:9120"
        );
        assert_eq!(
            super::ava1_cmds::ava1_addr("192.168.86.100:9114"),
            "192.168.86.100:9120"
        );
        assert_eq!(
            super::ava1_cmds::ava1_addr("192.168.86.100"),
            "192.168.86.100:9120"
        );
    }

    #[test]
    fn a_console_is_named_by_its_host_whatever_port_the_caller_typed() {
        for typed in [
            "10.0.0.5",
            "10.0.0.5:9113",
            "10.0.0.5:9114",
            "10.0.0.5:9120",
        ] {
            assert_eq!(super::console_host(typed), "10.0.0.5", "{typed}");
        }
        assert_eq!(super::console_host("[::1]:9114"), "[::1]");
        assert_eq!(super::console_host("[::1]"), "[::1]");
        assert_eq!(super::console_host("fe80::1"), "fe80::1");
        assert_eq!(super::console_host("ps5.lan:9113"), "ps5.lan");
    }
}
