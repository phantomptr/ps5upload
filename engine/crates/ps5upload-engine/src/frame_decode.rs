//! One picture from the live feed, for a frame request with no app page attached (rp_automation.rs).
//!
//! The engine has no video decoder of its own. It hands one keyframe (with its codec parameter
//! sets) to the ffmpeg Go live uses (ffmpeg_tool.rs), which writes a PNG or JPEG and exits. HDR
//! streams are not tone mapped: the picture comes out flat, but readable.

use std::process::Stdio;
use std::time::Duration;

use tokio::io::AsyncWriteExt;

use crate::ffmpeg_tool::FfmpegCmd;

/// One decode. A keyframe takes well under a second; this is for a stuck process.
const DECODE_WAIT: Duration = Duration::from_secs(5);

fn args(hevc: bool, format: &str, max_width: Option<u32>) -> Vec<String> {
    let mut a: Vec<String> = [
        "-hide_banner",
        "-loglevel",
        "error",
        "-f",
        if hevc { "hevc" } else { "h264" },
        "-i",
        "pipe:0",
        "-frames:v",
        "1",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    if let Some(w) = max_width {
        a.push("-vf".into());
        // Never wider than it came; an even height, which JPEG's 4:2:0 needs.
        a.push(format!("scale='min({w},iw)':-2"));
    }
    let tail: &[&str] = if format == "jpeg" {
        &[
            "-pix_fmt",
            "yuvj420p",
            "-c:v",
            "mjpeg",
            "-q:v",
            "3",
            "-f",
            "image2pipe",
            "pipe:1",
        ]
    } else {
        &[
            "-pix_fmt",
            "rgb24",
            "-c:v",
            "png",
            "-f",
            "image2pipe",
            "pipe:1",
        ]
    };
    a.extend(tail.iter().map(|s| s.to_string()));
    a
}

/// The picture in `bitstream` (Annex-B, starting at a keyframe with its parameter sets).
pub(crate) async fn decode(
    ff: &FfmpegCmd,
    bitstream: Vec<u8>,
    hevc: bool,
    format: &str,
    max_width: Option<u32>,
) -> Result<Vec<u8>, String> {
    let mut cmd = ff.command();
    cmd.args(args(hevc, format, max_width))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let mut child = cmd.spawn().map_err(|e| format!("starting ffmpeg: {e}"))?;
    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| "ffmpeg took no input".to_string())?;
    // Written on its own task: ffmpeg's output must be read meanwhile. Closing its input is what
    // makes the decoder give the picture up.
    tokio::spawn(async move {
        let _ = stdin.write_all(&bitstream).await;
        let _ = stdin.shutdown().await;
    });
    let out = tokio::time::timeout(DECODE_WAIT, child.wait_with_output())
        .await
        .map_err(|_| "ffmpeg took too long".to_string())?
        .map_err(|e| format!("running ffmpeg: {e}"))?;
    if !out.status.success() || out.stdout.is_empty() {
        let why = String::from_utf8_lossy(&out.stderr);
        let line = why.lines().last().unwrap_or("no output");
        return Err(format!("ffmpeg could not make the picture: {line}"));
    }
    Ok(out.stdout)
}

/// The width and height a PNG or JPEG says it has.
pub(crate) fn image_size(format: &str, b: &[u8]) -> Option<(u32, u32)> {
    if format == "png" {
        // Signature (8), IHDR length and name (8), then width and height.
        if b.len() < 24 || &b[12..16] != b"IHDR" {
            return None;
        }
        let w = u32::from_be_bytes(b[16..20].try_into().ok()?);
        let h = u32::from_be_bytes(b[20..24].try_into().ok()?);
        return Some((w, h));
    }
    // JPEG: walk the markers to the frame header (SOF0-SOF15 but DHT, JPG and DAC).
    let mut i = 2;
    while i + 9 < b.len() {
        if b[i] != 0xff {
            return None;
        }
        let m = b[i + 1];
        if m == 0xff {
            i += 1;
            continue;
        }
        if (0xc0..=0xcf).contains(&m) && !matches!(m, 0xc4 | 0xc8 | 0xcc) {
            let h = u16::from_be_bytes([b[i + 5], b[i + 6]]) as u32;
            let w = u16::from_be_bytes([b[i + 7], b[i + 8]]) as u32;
            return Some((w, h));
        }
        let len = u16::from_be_bytes([b[i + 2], b[i + 3]]) as usize;
        i += 2 + len;
    }
    None
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    #[test]
    fn asks_ffmpeg_for_one_picture_of_the_right_kind() {
        let a = args(false, "png", None).join(" ");
        assert!(a.contains("-f h264 -i pipe:0 -frames:v 1"), "{a}");
        assert!(a.contains("-c:v png"), "{a}");
        assert!(!a.contains("-vf"), "{a}");
        let a = args(true, "jpeg", Some(640)).join(" ");
        assert!(a.contains("-f hevc"), "{a}");
        assert!(a.contains("-c:v mjpeg"), "{a}");
        assert!(a.contains("scale='min(640,iw)':-2"), "{a}");
    }

    #[test]
    fn reads_an_images_size() {
        let mut png = vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a, 0, 0, 0, 13];
        png.extend_from_slice(b"IHDR");
        png.extend_from_slice(&1920u32.to_be_bytes());
        png.extend_from_slice(&1080u32.to_be_bytes());
        assert_eq!(image_size("png", &png), Some((1920, 1080)));
        assert_eq!(image_size("png", b"not a png"), None);

        let jpeg = [
            0xff, 0xd8, // SOI
            0xff, 0xe0, 0x00, 0x04, 0x00, 0x00, // APP0, 2 bytes of data
            0xff, 0xc0, 0x00, 0x11, 0x08, 0x02, 0xd0, 0x05, 0x00,
            0x03, // SOF0: 720 high, 1280 wide
        ];
        assert_eq!(image_size("jpeg", &jpeg), Some((1280, 720)));
        assert_eq!(image_size("jpeg", &[0xff, 0xd8, 0x00]), None);
    }

    /// The ffmpeg on this machine, for the tests that need a real one (ignored by default; CI's
    /// Go live job runs them).
    pub(crate) fn real_ffmpeg() -> FfmpegCmd {
        FfmpegCmd {
            program: std::env::var_os("PS5UPLOAD_FFMPEG")
                .map(Into::into)
                .unwrap_or_else(|| "ffmpeg".into()),
            lib_dir: None,
        }
    }

    /// One keyframe of a test picture, in Annex-B.
    pub(crate) async fn test_keyframe(
        ff: &FfmpegCmd,
        encoder: &str,
        muxer: &str,
    ) -> Option<Vec<u8>> {
        let out = ff
            .command()
            .args([
                "-hide_banner",
                "-loglevel",
                "error",
                "-f",
                "lavfi",
                "-i",
                "testsrc=size=640x360:rate=30",
                "-frames:v",
                "1",
                "-pix_fmt",
                "yuv420p",
                "-c:v",
                encoder,
                "-f",
                muxer,
                "pipe:1",
            ])
            .stderr(Stdio::null())
            .output()
            .await
            .ok()?;
        (out.status.success() && !out.stdout.is_empty()).then_some(out.stdout)
    }

    #[tokio::test]
    #[ignore = "needs ffmpeg"]
    async fn ffmpeg_makes_a_picture_of_a_keyframe() {
        let ff = real_ffmpeg();
        let au = test_keyframe(&ff, "libx264", "h264")
            .await
            .expect("libx264");
        let png = decode(&ff, au.clone(), false, "png", None).await.unwrap();
        assert_eq!(image_size("png", &png), Some((640, 360)));
        let jpeg = decode(&ff, au, false, "jpeg", Some(320)).await.unwrap();
        assert_eq!(image_size("jpeg", &jpeg), Some((320, 180)));
        // A build without an HEVC encoder is still fine for decoding: only then is this skipped.
        if let Some(au) = test_keyframe(&ff, "libx265", "hevc").await {
            let png = decode(&ff, au, true, "png", Some(9999)).await.unwrap();
            assert_eq!(image_size("png", &png), Some((640, 360)), "never upscaled");
        }
    }

    #[tokio::test]
    #[ignore = "needs ffmpeg"]
    async fn ffmpeg_says_why_garbage_makes_no_picture() {
        let e = decode(&real_ffmpeg(), vec![0u8; 64], false, "png", None)
            .await
            .unwrap_err();
        assert!(e.contains("ffmpeg"), "{e}");
    }
}
