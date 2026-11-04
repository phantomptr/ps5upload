//! Tests for the ffmpeg download: the pin table, what is unpacked, and that nothing unverified
//! is installed or run.

use super::*;

fn tmp() -> PathBuf {
    let d = std::env::temp_dir().join(format!("ps5upload-ffmpeg-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn sha(b: &[u8]) -> String {
    hex(&Sha256::digest(b))
}

#[test]
fn every_desktop_platform_has_a_pinned_build() {
    for (os, arch) in [
        ("windows", "x86_64"),
        ("windows", "aarch64"),
        ("linux", "x86_64"),
        ("linux", "aarch64"),
        ("macos", "aarch64"),
        ("macos", "x86_64"),
    ] {
        let p = pin_for(os, arch).unwrap_or_else(|| panic!("{os} {arch}"));
        assert!(p.url.starts_with("https://"), "{}", p.url);
        assert_eq!(p.sha256.len(), 64);
        assert!(p.sha256.chars().all(|c| c.is_ascii_hexdigit()));
        assert!(p.bytes > 10_000_000);
        assert!(p.url.contains("9.0.2"), "{}", p.url);
        // GPL builds (our code is GPL-3.0-or-later); never "latest" links that move.
        assert!(!p.url.contains("latest"), "{}", p.url);
        if os != "macos" {
            assert!(p.url.contains("-gpl-shared-"), "{}", p.url);
        }
    }
    assert!(pin_for("android", "aarch64").is_none());
}

#[test]
fn only_ffmpeg_and_its_libraries_are_unpacked() {
    let p = |s: &str| place(s).map(|p| p.to_string_lossy().replace('\\', "/"));
    // BtbN Windows
    let w = "ffmpeg-n9.0.2-17-g2a571b6068-win64-gpl-shared-9.0";
    assert_eq!(
        p(&format!("{w}/bin/ffmpeg.exe")).as_deref(),
        Some("bin/ffmpeg.exe")
    );
    assert_eq!(
        p(&format!("{w}/bin/avcodec-62.dll")).as_deref(),
        Some("bin/avcodec-62.dll")
    );
    assert_eq!(p(&format!("{w}/bin/ffplay.exe")), None);
    assert_eq!(p(&format!("{w}/bin/ffprobe.exe")), None);
    assert_eq!(p(&format!("{w}/include/libavcodec/avcodec.h")), None);
    assert_eq!(p(&format!("{w}/lib/avcodec.lib")), None);
    assert_eq!(p(&format!("{w}/doc/ffmpeg.html")), None);
    // BtbN Linux
    let l = "ffmpeg-n9.0.2-17-g2a571b6068-linux64-gpl-shared-9.0";
    assert_eq!(p(&format!("{l}/bin/ffmpeg")).as_deref(), Some("bin/ffmpeg"));
    assert_eq!(
        p(&format!("{l}/lib/libavcodec.so.62")).as_deref(),
        Some("lib/libavcodec.so.62")
    );
    assert_eq!(p(&format!("{l}/lib/pkgconfig/libavcodec.pc")), None);
    // macOS zip: the binary alone
    assert_eq!(p("ffmpeg").as_deref(), Some("bin/ffmpeg"));
    // Nothing outside the folder, nothing deeper.
    assert_eq!(p("../bin/ffmpeg"), None);
    assert_eq!(p("/bin/ffmpeg"), None);
    assert_eq!(p("a/b/bin/ffmpeg"), None);
    assert_eq!(p(&format!("{l}/bin/sub/ffmpeg")), None);
}

/// A tar header block for `name` (`kind` b'0' file, b'2' symlink, b'L' long name).
fn tar_header(name: &str, size: usize, kind: u8, link: &str) -> [u8; 512] {
    let mut h = [0u8; 512];
    h[..name.len().min(100)].copy_from_slice(&name.as_bytes()[..name.len().min(100)]);
    h[100..108].copy_from_slice(b"0000755\0");
    h[124..136].copy_from_slice(format!("{size:011o}\0").as_bytes());
    h[156] = kind;
    h[157..157 + link.len()].copy_from_slice(link.as_bytes());
    h[257..263].copy_from_slice(b"ustar\0");
    h[148..156].copy_from_slice(b"        ");
    let sum: u32 = h.iter().map(|&b| b as u32).sum();
    h[148..156].copy_from_slice(format!("{sum:06o}\0 ").as_bytes());
    h
}

fn tar_file(out: &mut Vec<u8>, name: &str, data: &[u8]) {
    out.extend_from_slice(&tar_header(name, data.len(), b'0', ""));
    out.extend_from_slice(data);
    out.resize(out.len().div_ceil(512) * 512, 0);
}

#[test]
fn tar_files_symlinks_and_long_names() {
    let top = "ffmpeg-n9.0.2-17-g2a571b6068-linuxarm64-gpl-shared-9.0";
    let mut t = Vec::new();
    tar_file(&mut t, &format!("{top}/bin/ffmpeg"), b"#!ffmpeg");
    tar_file(&mut t, &format!("{top}/bin/ffplay"), b"nope");
    tar_file(
        &mut t,
        &format!("{top}/lib/libavcodec.so.62.1.100"),
        &[1u8; 1000],
    );
    t.extend_from_slice(&tar_header(
        &format!("{top}/lib/libavcodec.so.62"),
        0,
        b'2',
        "libavcodec.so.62.1.100",
    ));
    // A link out of the folder is not made.
    t.extend_from_slice(&tar_header(
        &format!("{top}/lib/libevil.so"),
        0,
        b'2',
        "../../etc/passwd",
    ));
    // A GNU long name (over 100 bytes).
    let long = format!("{top}/lib/libswresample.so.6.0.100-with-a-very-long-suffix-that-goes-on");
    assert!(long.len() > 100);
    let mut data = long.as_bytes().to_vec();
    data.push(0);
    t.extend_from_slice(&tar_header("././@LongLink", data.len(), b'L', ""));
    t.extend_from_slice(&data);
    t.resize(t.len().div_ceil(512) * 512, 0);
    tar_file(&mut t, &long[..100], b"swr");
    t.extend_from_slice(&[0u8; 1024]);

    let dir = tmp();
    unpack_tar(&t[..], &dir).unwrap();
    assert_eq!(std::fs::read(dir.join("bin/ffmpeg")).unwrap(), b"#!ffmpeg");
    assert!(!dir.join("bin/ffplay").exists());
    assert_eq!(
        std::fs::read(dir.join("lib/libavcodec.so.62"))
            .unwrap()
            .len(),
        1000,
        "the symlink resolves to the library"
    );
    assert!(!dir.join("lib/libevil.so").exists());
    let short_name = long.rsplit('/').next().unwrap();
    assert_eq!(
        std::fs::read(dir.join("lib").join(short_name)).unwrap(),
        b"swr"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let m = std::fs::metadata(dir.join("bin/ffmpeg"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(m & 0o111, 0o111);
    }
    // A cut-off archive is an error, not a half install.
    assert!(unpack_tar(&t[..700], &tmp()).is_err());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn tar_xz_round_trip() {
    // xz-compress a small tar with lzma-rust2's encoder? It is not built (decoder only), so
    // use the system's xz if present; skip otherwise.
    let Ok(out) = std::process::Command::new("xz").arg("--version").output() else {
        return;
    };
    if !out.status.success() {
        return;
    }
    let mut t = Vec::new();
    tar_file(&mut t, "top/bin/ffmpeg", b"binary");
    t.extend_from_slice(&[0u8; 1024]);
    let dir = tmp();
    let tar = dir.join("a.tar");
    std::fs::write(&tar, &t).unwrap();
    assert!(std::process::Command::new("xz")
        .arg("-k")
        .arg(&tar)
        .status()
        .unwrap()
        .success());
    let to = dir.join("out");
    std::fs::create_dir_all(&to).unwrap();
    unpack_tar_xz(&dir.join("a.tar.xz"), &to).unwrap();
    assert_eq!(std::fs::read(to.join("bin/ffmpeg")).unwrap(), b"binary");
    let _ = std::fs::remove_dir_all(&dir);
}

fn zip_with(files: &[(&str, &[u8])]) -> Vec<u8> {
    let mut buf = std::io::Cursor::new(Vec::new());
    {
        let mut z = zip::ZipWriter::new(&mut buf);
        let o = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);
        for (n, d) in files {
            z.start_file(*n, o).unwrap();
            z.write_all(d).unwrap();
        }
        z.finish().unwrap();
    }
    buf.into_inner()
}

fn leak(s: String) -> &'static str {
    Box::leak(s.into_boxed_str())
}

#[test]
fn install_verifies_before_unpacking_and_find_checks_the_binary() {
    let zip = zip_with(&[
        (exe_name(), b"\x7fELF-pretend-ffmpeg"),
        ("README.txt", b"hi"),
    ]);
    let pin = Pin {
        os: "test",
        arch: "test",
        url: "https://example.invalid/ffmpeg.zip",
        sha256: leak(sha(&zip)),
        bytes: zip.len() as u64,
        archive: Archive::Zip,
    };
    let root = tmp();
    let dir = root.join("tools").join("ffmpeg-9.0.2");

    // A download that is not the pinned file: refused, nothing installed, nothing left over.
    let wrong = {
        let mut z = zip.clone();
        z[40] ^= 1;
        z
    };
    let fetch_wrong = move |_: &str, w: &mut dyn Write, p: &dyn Fn(u64)| {
        w.write_all(&wrong).map_err(|e| e.to_string())?;
        p(wrong.len() as u64);
        Ok(())
    };
    let e = install(&pin, &dir, &fetch_wrong).unwrap_err();
    assert_eq!(e.0, "checksum");
    assert!(!dir.exists());
    assert!(std::fs::read_dir(root.join("tools"))
        .unwrap()
        .next()
        .is_none());
    assert!(find_in(&dir).is_none());

    // A failed download: nothing either.
    let fail = |_: &str, _: &mut dyn Write, _: &dyn Fn(u64)| Err("offline".to_string());
    assert_eq!(install(&pin, &dir, &fail).unwrap_err().0, "download");
    assert!(!dir.exists());

    // The right file: installed, with its record.
    let good = zip.clone();
    let fetch = move |_: &str, w: &mut dyn Write, _: &dyn Fn(u64)| {
        w.write_all(&good).map_err(|e| e.to_string())
    };
    install(&pin, &dir, &fetch).unwrap();
    let found = find_in(&dir).expect("installed");
    assert_eq!(found.program, dir.join("bin").join(exe_name()));
    assert!(!dir.join("README.txt").exists());
    // Swapped afterwards: not run. (A new length makes the check run again.)
    std::fs::write(found.program, b"\x7fELF-something-else-entirely").unwrap();
    assert!(find_in(&dir).is_none());
    let _ = std::fs::remove_dir_all(&root);
}

/// The Linux builds start through glibc's loader: a `FROM scratch` Docker image has none, so the
/// download could only ever fail with "No such file or directory" there.
#[test]
fn a_build_that_cannot_start_here_is_not_offered() {
    let linux = pin_for("linux", "x86_64").unwrap();
    assert!(!runs_with(linux, |_| false), "no loader: can't run");
    assert!(runs_with(linux, |p| {
        p == Path::new("/lib64/ld-linux-x86-64.so.2")
    }));
    let arm = pin_for("linux", "aarch64").unwrap();
    assert!(runs_with(arm, |p| p == Path::new("/lib/ld-linux-aarch64.so.1")));
    assert!(!runs_with(arm, |p| {
        p == Path::new("/lib64/ld-linux-x86-64.so.2")
    }));
    // Nothing of the sort on the others.
    for (os, arch) in [
        ("macos", "aarch64"),
        ("macos", "x86_64"),
        ("windows", "x86_64"),
        ("windows", "aarch64"),
    ] {
        assert!(runs_with(pin_for(os, arch).unwrap(), |_| false), "{os}");
    }
}

#[test]
fn a_new_version_removes_the_old_ones() {
    let tools = tmp();
    let keep = tools.join("ffmpeg-9.0.2");
    let dirs = [
        "ffmpeg-9.0.2",
        "ffmpeg-8.0.1",
        "ffmpeg-7.1",
        "ffmpeg-mine",
        "other",
    ];
    for d in dirs {
        std::fs::create_dir_all(tools.join(d).join("bin")).unwrap();
        std::fs::write(tools.join(d).join("bin").join("ffmpeg"), b"x").unwrap();
    }
    std::fs::write(tools.join("ffmpeg-notes.txt"), b"a file").unwrap();
    // PS5UPLOAD_FFMPEG runs from this one: the person's choice.
    let own = tools.join("ffmpeg-mine").join("bin").join("ffmpeg");
    remove_old_versions(&tools, &keep, Some(&own));
    assert!(keep.join("bin/ffmpeg").exists());
    assert!(own.exists());
    assert!(tools.join("other/bin/ffmpeg").exists(), "not ours");
    assert!(tools.join("ffmpeg-notes.txt").exists(), "not a folder");
    assert!(!tools.join("ffmpeg-8.0.1").exists());
    assert!(!tools.join("ffmpeg-7.1").exists());
    let _ = std::fs::remove_dir_all(&tools);
}

/// The real thing: this computer's pinned build, downloaded, verified, unpacked and run.
#[test]
#[ignore = "downloads ffmpeg (30-90 MB) from the pinned URL"]
fn the_pinned_build_for_this_computer_installs_and_runs() {
    let pin = this_pin().expect("a pin for this computer");
    let root = tmp();
    let dir = root.join("tools").join("ffmpeg-9.0.2");
    install(pin, &dir, &download).unwrap();
    let ff = find_in(&dir).expect("verified");
    let out = std::process::Command::new(&ff.program)
        .arg("-hide_banner")
        .arg("-protocols")
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    eprintln!(
        "{}",
        text.lines()
            .filter(|l| l.contains("rtmp") || l.contains("srt"))
            .collect::<Vec<_>>()
            .join(" ")
    );
    assert!(text.contains("rtmps"), "rtmps support");
    let v = std::process::Command::new(&ff.program)
        .arg("-version")
        .output()
        .unwrap();
    let v = String::from_utf8_lossy(&v.stdout);
    eprintln!("{}", v.lines().next().unwrap_or(""));
    assert!(v.contains("9.0.2"));
    let _ = std::fs::remove_dir_all(&root);
}

/// BtbN's Linux build (a .tar.xz with symlinked libraries) unpacks as planned, on any OS.
#[test]
#[ignore = "downloads ffmpeg for Linux arm64 (58 MB)"]
fn the_pinned_linux_build_unpacks() {
    let pin = pin_for("linux", "aarch64").unwrap();
    let root = tmp();
    let dir = root.join("tools").join("ffmpeg-9.0.2");
    install(pin, &dir, &download).unwrap();
    let base = dir.clone();
    let libs: Vec<String> = std::fs::read_dir(base.join("lib"))
        .map(|d| {
            d.filter_map(|e| e.ok())
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    eprintln!(
        "bin: {:?}",
        std::fs::read_dir(base.join("bin")).map(|d| d
            .filter_map(|e| e.ok())
            .map(|e| e.file_name())
            .collect::<Vec<_>>())
    );
    eprintln!("lib: {libs:?}");
    assert!(base.join("bin/ffmpeg").exists());
    assert!(libs.iter().any(|l| l.starts_with("libavcodec.so")));
    let _ = std::fs::remove_dir_all(&root);
}

/// rtmps:// (YouTube) needs trusted roots the builds' OpenSSL can't find by itself.
#[cfg(target_os = "macos")]
#[test]
fn ffmpeg_gets_the_system_ca_bundle() {
    let ca = system_ca_bundle().expect("macOS has /etc/ssl/cert.pem");
    assert_eq!(ca, std::path::Path::new("/etc/ssl/cert.pem"));
    if std::env::var_os("SSL_CERT_FILE").is_none() {
        let cmd = FfmpegCmd {
            program: "ffmpeg".into(),
            lib_dir: None,
        };
        let c = cmd.command();
        let set = c
            .as_std()
            .get_envs()
            .find(|(k, _)| *k == "SSL_CERT_FILE")
            .and_then(|(_, v)| v.map(|v| v.to_owned()));
        assert_eq!(set.as_deref(), Some(ca.as_os_str()));
    }
}
