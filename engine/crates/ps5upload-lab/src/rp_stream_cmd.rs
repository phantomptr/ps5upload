//! `rp-stream <creds.json> <out> [seconds] [h264|h265|h265_hdr] [WxH]`: open a Remote
//! Play stream with a paired console and write its video (Annex-B) to a file,
//! timing each milestone. `creds.json` is `{"<host>": {"regist_key": hex32,
//! "rp_key": hex32}}`; the keys are never printed.

use std::fs::File;
use std::io::Write as _;
use std::time::{Duration, Instant};

use anyhow::{anyhow, Result};
use ps5upload_core::rp_session::SessionCreds;
use ps5upload_core::rp_stream::{run_stream, Codec, Progress, StreamConfig, StreamSink};

struct FileSink {
    out: File,
    t0: Instant,
    units: u64,
    audio_frames: u64,
}

impl StreamSink for FileSink {
    fn progress(&mut self, p: Progress) {
        let ms = self.t0.elapsed().as_millis();
        match p {
            Progress::Waking => eprintln!("[{ms:>6} ms] console in rest mode: waking it"),
            Progress::Stats(_) => {}
            Progress::Nonce => eprintln!("[{ms:>6} ms] session-init: nonce"),
            Progress::SessionId => eprintln!("[{ms:>6} ms] ctrl: session id"),
            Progress::TakionConnected => eprintln!("[{ms:>6} ms] takion: connected"),
            Progress::Bang => eprintln!("[{ms:>6} ms] stream: BANG, crypt on"),
            Progress::StreamInfo(r) => eprintln!("[{ms:>6} ms] stream: STREAMINFO {r:?}"),
            Progress::FirstFrame(n) => eprintln!("[{ms:>6} ms] video: first frame ({n} bytes)"),
        }
    }

    fn video(&mut self, au: &[u8], _header: bool) -> bool {
        self.units += 1;
        self.out.write_all(au).is_ok()
    }

    fn audio(&mut self, frame: &[u8], header: bool) -> bool {
        if header {
            eprintln!(
                "audio header: {:?}",
                ps5upload_core::rp_stream::AudioHeader::parse(frame)
            );
        } else {
            if self.audio_frames == 0 {
                eprintln!(
                    "first audio frame: index {}, {} bytes of Opus",
                    u16::from_be_bytes([frame[0], frame[1]]),
                    frame.len() - 2
                );
            }
            self.audio_frames += 1;
        }
        true
    }
}

pub fn run(host: &str, args: &[String]) -> Result<()> {
    let creds_path = args.first().ok_or_else(|| anyhow!("need creds.json"))?;
    let out_path = args.get(1).ok_or_else(|| anyhow!("need an output file"))?;
    let secs: u64 = args.get(2).map(|s| s.parse()).transpose()?.unwrap_or(10);
    let mut cfg = StreamConfig::default();
    if let Some(c) = args.get(3).and_then(|s| Codec::from_name(s)) {
        cfg.codec = c;
    }
    if let Some((w, h)) = args.get(4).and_then(|s| s.split_once('x')) {
        cfg.width = w.parse()?;
        cfg.height = h.parse()?;
        if cfg.height >= 1080 {
            cfg.bitrate_kbps = 15000;
        }
    }
    let all: serde_json::Value = serde_json::from_slice(&std::fs::read(creds_path)?)?;
    let c = &all[host];
    let creds = SessionCreds::from_hex(
        c["regist_key"].as_str().unwrap_or(""),
        c["rp_key"].as_str().unwrap_or(""),
    )?;
    let mut sink = FileSink {
        out: File::create(out_path)?,
        t0: Instant::now(),
        units: 0,
        audio_frames: 0,
    };
    let report = run_stream(host, &creds, cfg, Duration::from_secs(secs), &mut sink)?;
    let secs_f = sink.t0.elapsed().as_secs_f64();
    eprintln!(
        "done in {secs_f:.1}s: {} frames ({} rebuilt by FEC, {} lost), {:.1} MB video, {} audio frames, \
         {} AV packets ({} bad MAC, {} dropped on purpose), {:.1} Mbit/s on the wire, disconnect: {:?}",
        report.video.frames,
        report.video.frames_recovered,
        report.video.frames_dropped,
        report.video.bytes as f64 / 1e6,
        sink.audio_frames,
        report.av_packets,
        report.av_bad_mac,
        report.av_dropped_on_purpose,
        report.bytes_received as f64 * 8.0 / 1e6 / secs_f,
        report.disconnect_reason
    );
    eprintln!("{} access units written to {out_path}", sink.units);
    Ok(())
}

/// `app-running` / `app-kill <app_id>` / `app-close <TITLE_ID>`. app-close is
/// the desktop's "Close game": the app id from the process list (the
/// libSceSysCore list is blind to running games), a graceful kill, then the
/// process itself if that is refused. For tests that open a game.
pub fn app_cmd(host: &str, args: &[String]) -> Result<()> {
    use ps5upload_core::app_lifecycle::{app_lifecycle, AppAction};
    if args.first().map(String::as_str) == Some("app-close") {
        let title = args.get(1).ok_or_else(|| anyhow!("need a title id"))?;
        let procs = ps5upload_core::process_mgr::process_list(host)?;
        let Some(p) = procs
            .processes
            .iter()
            .find(|p| p.kind == "app" && p.title_id.eq_ignore_ascii_case(title))
        else {
            println!("{title} is not running");
            return Ok(());
        };
        let kill = app_lifecycle(host, AppAction::Kill, p.app_id);
        println!("app kill (app id {:#x}): {kill:?}", p.app_id);
        std::thread::sleep(Duration::from_secs(3));
        let still = ps5upload_core::process_mgr::process_list(host)?
            .processes
            .iter()
            .any(|q| q.pid == p.pid);
        if still {
            println!("still running; killing pid {}", p.pid);
            println!(
                "{:?}",
                ps5upload_core::process_mgr::process_kill(host, p.pid)?
            );
        } else {
            println!("{title} closed");
        }
        return Ok(());
    }
    let (action, id) = match args.first().map(String::as_str) {
        Some("app-kill") => (
            AppAction::Kill,
            args.get(1)
                .ok_or_else(|| anyhow!("need an app id"))?
                .parse()?,
        ),
        _ => (AppAction::List, 0),
    };
    let r = app_lifecycle(host, action, id)?;
    println!("{}", serde_json::to_string_pretty(&r)?);
    Ok(())
}

/// `rp-ddp`: the console's raw discovery reply (UDP 9302). Takes no session,
/// so it is safe to poll while a Remote Play session may be held.
pub fn ddp_raw(host: &str) -> Result<()> {
    let sock = std::net::UdpSocket::bind("0.0.0.0:0")?;
    sock.set_read_timeout(Some(Duration::from_secs(2)))?;
    sock.send_to(
        b"SRCH * HTTP/1.1\ndevice-discovery-protocol-version:00030010\n\0",
        (host, 9302),
    )?;
    let mut buf = [0u8; 2048];
    let n = sock
        .recv(&mut buf)
        .map_err(|e| anyhow!("no DDP reply: {e}"))?;
    println!("{}", String::from_utf8_lossy(&buf[..n]));
    Ok(())
}
