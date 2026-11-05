//! PS5 screenshot (`.jxr` / JPEG XR) → PNG conversion (desktop only).
//!
//! ## Why this exists
//! PS5 saves screenshots as JPEG XR (`.jxr`) because captures are **HDR**:
//! Rec.2100 colour primaries with the ST.2084 (PQ) transfer function, more
//! than 8 bits per channel — none of which ordinary JPEG/PNG can hold. The
//! cost is that `.jxr` won't open in macOS Preview, most browsers, or the
//! Windows photo viewer without flaky extensions. This module decodes the
//! `.jxr`, tone-maps the HDR content down to a normal SDR image, and writes
//! a PNG the user can open anywhere.
//!
//! ## Pipeline
//! ```text
//!   decode (jxrlib via the `jpegxr` crate)
//!     → per-pixel code values, normalised to [0,1] (or float as-is)
//!     → branch on the decoded pixel format:
//!         • 8-bit SDR formats  → already display-ready, just repack to RGB8
//!         • ≥10-bit integer    → PQ EOTF → linear Rec.2020 → Rec.709 → tone-map
//!         • float (scRGB)      → linear Rec.709 → tone-map
//!     → sRGB OETF (gamma) → 8-bit
//!   encode PNG (`image` crate)
//! ```
//!
//! ## Tone-mapping
//! Per-channel **extended Reinhard** with the white point taken from the
//! image's own peak. This is deliberately parameter-light and content-
//! adaptive: the brightest pixel maps to 1.0 (nothing clips/blows out) and
//! mid-tones are preserved, so the result looks natural on any SDR screen
//! without per-image tuning. (hdrfix-style filmic curves like Hable/ACES
//! look great but need hand-tuned exposure per content; Reinhard-extended
//! is the safe default for an unattended one-click convert.)
//!
//! ## Validation note
//! There is no sample PS5 `.jxr` in the repo, so the colour math below is
//! coded to the documented PS5 spec (Rec.2100 PQ) and unit-tested against
//! known PQ/sRGB reference values — but the *exact* native pixel format
//! jxrlib hands back for a real PS5 capture (10-bit packed vs 16-bit) is
//! confirmed on the first real run: an unrecognised format returns a clear
//! error naming the format so we can add it. See `convert_jxr_to_png`.

/// Tauri command: convert a local `.jxr` file at `src_path` into a PNG at
/// `dst_path`. Both are absolute filesystem paths on the user's machine.
///
/// When `delete_source` is true the intermediate `.jxr` is removed after a
/// *successful* conversion (the Convert flow downloads the raw `.jxr` then
/// converts, and the user only wanted the viewable PNG). On failure the
/// `.jxr` is left in place so the user can retry or report it.
///
/// Runs the (CPU-bound) decode+tonemap on a blocking thread so the async
/// runtime/UI isn't stalled. Returns `dst_path` on success.
#[cfg(not(any(
    target_os = "android",
    target_os = "ios",
    all(target_os = "windows", target_arch = "aarch64")
)))]
#[tauri::command]
pub async fn screenshot_convert(
    src_path: String,
    dst_path: String,
    delete_source: bool,
    // `sdr` (default: tone-mapped 8-bit sRGB), `hdr` (16-bit PQ BT.2020 with a cICP chunk) or
    // `hdr_plain` (the same without the chunk, for viewers that choke on it; #448).
    mode: Option<String>,
) -> Result<String, String> {
    let mode = mode.unwrap_or_else(|| "sdr".to_string());
    tokio::task::spawn_blocking(move || {
        convert_in_child(&src_path, &dst_path, &mode)?;
        // Only remove the original after the PNG is safely written, and
        // never if it somehow resolves to the same path as the output.
        if delete_source && src_path != dst_path {
            let _ = std::fs::remove_file(&src_path);
        }
        Ok(dst_path)
    })
    .await
    .map_err(|e| format!("convert task: {e}"))?
}

/// The argument that runs this executable as a one-shot decoder (see `child_mode`).
#[cfg(not(any(
    target_os = "android",
    target_os = "ios",
    all(target_os = "windows", target_arch = "aarch64")
)))]
const CHILD_ARG: &str = "--ps5upload-jxr-to-png";

/// Decode in a child process, never in the app's own. jxrlib is C, and a corrupt capture
/// with a valid header made it segfault, which took the whole app down and, with Captures
/// the last open tab, crash-looped it on every launch (#229, again in #413: the header
/// check alone did not catch it). A crash in the child is one failed thumbnail.
#[cfg(not(any(
    target_os = "android",
    target_os = "ios",
    all(target_os = "windows", target_arch = "aarch64")
)))]
fn convert_in_child(src_path: &str, dst_path: &str, mode: &str) -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| format!("locate the app: {e}"))?;
    let mut decoder = std::process::Command::new(exe);
    decoder.arg(CHILD_ARG).arg(src_path).arg(dst_path).arg(mode);
    let (status, stderr) = run_with_deadline(decoder, DECODE_DEADLINE)
        .map_err(|e| format!("start the decoder: {e}"))?
        .ok_or_else(|| super::shell_error::code_only("screenshot_decoder_timeout"))?;
    if status.success() {
        return Ok(());
    }
    let msg = stderr.trim().to_string();
    Err(if msg.is_empty() {
        // Killed by a signal: the codec crashed on this file.
        super::shell_error::coded(
            "screenshot_decoder_crashed",
            serde_json::json!({ "detail": status.to_string() }),
        )
    } else {
        msg
    })
}

/// The longest one capture may take to decode. A real one takes seconds; this stops a decoder
/// that loops on a corrupt file from holding its thread and the waiting thumbnail for the life of
/// the app, one per file. The machine may be busy and the file on a slow disk, so it is generous.
#[cfg(not(any(
    target_os = "android",
    target_os = "ios",
    all(target_os = "windows", target_arch = "aarch64")
)))]
const DECODE_DEADLINE: std::time::Duration = std::time::Duration::from_secs(60);

/// Run `cmd`, killing it at `deadline`: its exit status and what it wrote to stderr, or None
/// when the deadline passed.
#[cfg(not(any(
    target_os = "android",
    target_os = "ios",
    all(target_os = "windows", target_arch = "aarch64")
)))]
fn run_with_deadline(
    mut cmd: std::process::Command,
    deadline: std::time::Duration,
) -> std::io::Result<Option<(std::process::ExitStatus, String)>> {
    use std::io::Read;
    let mut child = cmd
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .spawn()?;
    // Read as it comes, on its own thread: a child that fills the pipe would never exit.
    let mut pipe = child.stderr.take();
    let reader = std::thread::spawn(move || {
        let mut text = Vec::new();
        if let Some(pipe) = pipe.as_mut() {
            let _ = pipe.read_to_end(&mut text);
        }
        String::from_utf8_lossy(&text).into_owned()
    });
    let started = std::time::Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break Some(status);
        }
        if started.elapsed() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            break None;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    };
    // Past the deadline nothing waits for the reader: a process the decoder started can hold the
    // pipe open long after the decoder is gone, and the thread ends with the pipe.
    Ok(status.map(|status| (status, reader.join().unwrap_or_default())))
}

/// Run as the one-shot decoder when started with `CHILD_ARG <src> <dst>`: convert, report any
/// error on stderr, and exit. Returns false (do nothing) for a normal launch. Called first
/// thing in `main`, before Tauri starts.
#[cfg(not(any(
    target_os = "android",
    target_os = "ios",
    all(target_os = "windows", target_arch = "aarch64")
)))]
pub fn child_mode() -> bool {
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(String::as_str) != Some(CHILD_ARG) || !(4..=5).contains(&args.len()) {
        return false;
    }
    let mode = desktop::Mode::parse(args.get(4).map(String::as_str));
    let result = std::fs::read(&args[2])
        .map_err(|e| format!("read {}: {e}", args[2]))
        .and_then(|bytes| desktop::convert_jxr_to_png(&bytes, mode))
        .and_then(|png| {
            std::fs::write(&args[3], png).map_err(|e| format!("write {}: {e}", args[3]))
        });
    match result {
        Ok(()) => std::process::exit(0),
        Err(e) => {
            use std::io::Write;
            let _ = writeln!(std::io::stderr(), "{e}");
            std::process::exit(1)
        }
    }
}

/// No decoder on these targets, so never a decoder child.
#[cfg(any(
    target_os = "android",
    target_os = "ios",
    all(target_os = "windows", target_arch = "aarch64")
))]
pub fn child_mode() -> bool {
    false
}

/// Mobile fallback: the JPEG XR codec is a desktop-only dependency, so on
/// Android/iOS the command exists (the shared frontend can always call it)
/// but reports that conversion isn't available there.
#[cfg(any(
    target_os = "android",
    target_os = "ios",
    all(target_os = "windows", target_arch = "aarch64")
))]
#[tauri::command]
pub async fn screenshot_convert(
    _src_path: String,
    _dst_path: String,
    _delete_source: bool,
    _mode: Option<String>,
) -> Result<String, String> {
    Err(super::shell_error::code_only("screenshot_unsupported"))
}

#[cfg(not(any(
    target_os = "android",
    target_os = "ios",
    all(target_os = "windows", target_arch = "aarch64")
)))]
mod desktop {
    use image::{ImageEncoder, ImageError};
    use jpegxr::{ImageDecode, PixelFormat, PixelInfo};
    use std::io::Cursor;

    /// What the PNG carries.
    #[derive(Clone, Copy, PartialEq, Eq, Debug)]
    pub enum Mode {
        /// Tone-mapped to SDR, 8-bit sRGB: opens everywhere.
        Sdr,
        /// 16-bit BT.2020 PQ, as captured, with a cICP chunk (primaries 9, transfer 16, matrix
        /// 0, full range) so HDR-aware viewers and browsers show it as HDR (#448).
        Hdr { cicp: bool },
    }

    impl Mode {
        pub fn parse(s: Option<&str>) -> Self {
            match s {
                Some("hdr") => Mode::Hdr { cicp: true },
                Some("hdr_plain") => Mode::Hdr { cicp: false },
                _ => Mode::Sdr,
            }
        }
    }

    /// Decode a JPEG XR byte buffer and return PNG-encoded bytes: RGB8 tone-mapped for
    /// [`Mode::Sdr`]; 16-bit PQ BT.2020 for [`Mode::Hdr`] when the capture is HDR (an SDR
    /// capture has nothing more to give and comes out as 8-bit sRGB either way).
    pub fn convert_jxr_to_png(jxr: &[u8], mode: Mode) -> Result<Vec<u8>, String> {
        // Validate the JPEG XR file signature before handing the buffer to
        // the C codec (jxrlib). A truncated or corrupted .jxr — common for
        // PS5 thumbnail variants that are still being written, or a
        // download that was cut short — can cause the native `copy_all` to
        // read past the buffer and SIGSEGV, taking the whole app down.
        // Rejecting non-JXR bytes here turns a hard crash into a clean
        // error the UI can surface. The signature is the 4-byte TIFF-style
        // marker: `II\xBC\x01` (little-endian) or `MM\xBC\x01` (big-endian).
        if jxr.len() < 8 {
            return Err(format!(
                "jxr too small ({} bytes) — likely a truncated download",
                jxr.len()
            ));
        }
        let is_jxr = (jxr[0] == 0x49 && jxr[1] == 0x49 && jxr[2] == 0xBC && jxr[3] == 0x01)
            || (jxr[0] == 0x4D && jxr[1] == 0x4D && jxr[2] == 0xBC && jxr[3] == 0x01);
        if !is_jxr {
            return Err(format!(
                "not a JPEG XR file (magic: {:02x?}) — the first 4 bytes \
                 don't match the JXR signature. This may be a regular JPEG \
                 or PNG mislabeled .jxr.",
                &jxr[..4]
            ));
        }
        if let Mode::Hdr { cicp } = mode {
            if let Some((width, height, rgb16)) = decode_to_pq16(jxr)? {
                // The capture's own ICC profile (Sony's "ScreenCapturePQ"): viewers that do
                // not read cICP, and HDR-capable browsers, show the image as HDR through it.
                let icc = jxr_icc_profile(jxr);
                return encode_png16(width, height, &rgb16, cicp, icc.as_deref());
            }
        }
        let (width, height, rgb8) = decode_to_rgb8(jxr)?;
        let mut out = Vec::new();
        image::codecs::png::PngEncoder::new(Cursor::new(&mut out))
            .write_image(&rgb8, width, height, image::ExtendedColorType::Rgb8)
            .map_err(|e: ImageError| format!("png encode: {e}"))?;
        Ok(out)
    }

    /// The ICC profile embedded in a JPEG XR container, if any. The container is TIFF-like:
    /// `II`/`MM`, 0xBC, version, then an IFD whose entries are (tag u16, type u16, count u32,
    /// value-or-offset u32); the profile is TIFF's InterColorProfile tag 0x8773 (JXRMeta.h's
    /// WMP_tagIccProfile), type UNDEFINED, its bytes at the offset.
    pub fn jxr_icc_profile(jxr: &[u8]) -> Option<Vec<u8>> {
        if jxr.len() < 8 || jxr[2] != 0xBC {
            return None;
        }
        let le = match &jxr[0..2] {
            b"II" => true,
            b"MM" => false,
            _ => return None,
        };
        let u16_at = |at: usize| -> Option<u16> {
            let b = jxr.get(at..at + 2)?;
            Some(if le {
                u16::from_le_bytes([b[0], b[1]])
            } else {
                u16::from_be_bytes([b[0], b[1]])
            })
        };
        let u32_at = |at: usize| -> Option<u32> {
            let b = jxr.get(at..at + 4)?;
            Some(if le {
                u32::from_le_bytes([b[0], b[1], b[2], b[3]])
            } else {
                u32::from_be_bytes([b[0], b[1], b[2], b[3]])
            })
        };
        let ifd = u32_at(4)? as usize;
        let entries = u16_at(ifd)? as usize;
        for i in 0..entries.min(4096) {
            let e = ifd + 2 + i * 12;
            if u16_at(e)? != 0x8773 {
                continue;
            }
            let count = u32_at(e + 4)? as usize;
            let at = u32_at(e + 8)? as usize;
            // Four bytes or fewer live in the entry itself; a profile never does.
            if count <= 4 || count > 16 << 20 {
                return None;
            }
            return jxr.get(at..at.checked_add(count)?).map(<[u8]>::to_vec);
        }
        None
    }

    /// A 16-bit RGB PNG (big-endian samples, as PNG stores them). Right after IHDR: the
    /// capture's ICC profile as iCCP when it has one, and a cICP chunk when asked (BT.2020
    /// primaries 9, PQ transfer 16, RGB 0, full range 1).
    fn encode_png16(
        width: u32,
        height: u32,
        rgb16: &[u16],
        cicp: bool,
        icc: Option<&[u8]>,
    ) -> Result<Vec<u8>, String> {
        let mut out = Vec::with_capacity(rgb16.len() * 2 / 2);
        {
            let mut enc = png::Encoder::new(&mut out, width, height);
            enc.set_color(png::ColorType::Rgb);
            enc.set_depth(png::BitDepth::Sixteen);
            let mut w = enc.write_header().map_err(|e| format!("png header: {e}"))?;
            if let Some(profile) = icc {
                w.write_chunk(png::chunk::ChunkType(*b"iCCP"), &iccp_chunk(profile))
                    .map_err(|e| format!("png iCCP: {e}"))?;
            }
            if cicp {
                w.write_chunk(png::chunk::ChunkType(*b"cICP"), &[9, 16, 0, 1])
                    .map_err(|e| format!("png cICP: {e}"))?;
            }
            let mut bytes = Vec::with_capacity(rgb16.len() * 2);
            for v in rgb16 {
                bytes.extend_from_slice(&v.to_be_bytes());
            }
            w.write_image_data(&bytes)
                .map_err(|e| format!("png data: {e}"))?;
            w.finish().map_err(|e| format!("png finish: {e}"))?;
        }
        Ok(out)
    }

    /// An iCCP chunk body: profile name, NUL, compression method 0, zlib-compressed profile.
    fn iccp_chunk(profile: &[u8]) -> Vec<u8> {
        use std::io::Write;
        let mut body = b"ScreenCapturePQ\0\0".to_vec();
        let mut z = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
        let _ = z.write_all(profile);
        body.extend(z.finish().unwrap_or_default());
        body
    }

    /// The capture as 16-bit BT.2020 PQ codes, or `None` for an SDR capture. A PS5 capture
    /// is PQ already (10 bits in a 16-bit word on a 4K capture, scaled to the full 16 bits);
    /// an scRGB float capture is converted to the same.
    fn decode_to_pq16(jxr: &[u8]) -> Result<Option<(u32, u32, Vec<u16>)>, String> {
        let (w, h, raw, bpp, kind) = decode_raw(jxr)?;
        let npix = w * h;
        let mut out: Vec<u16> = Vec::with_capacity(npix * 3);
        for p in 0..npix {
            let off = p * (bpp / 8);
            let px = &raw[off..off + bpp / 8];
            let (r, g, b) = match kind {
                Kind::Sdr8 { .. } => return Ok(None),
                Kind::PqInt { bits, channels } => read_int_rgb(px, bits, channels),
                Kind::ScRgbFloat { half, channels } => {
                    // Linear Rec.709 with 1.0 = SDR white (100 nits) → linear Rec.2020 as a
                    // share of 10000 nits → PQ code.
                    let lin = read_float_rgb(px, half, channels);
                    let c = rec709_to_rec2020([lin[0].max(0.0), lin[1].max(0.0), lin[2].max(0.0)]);
                    (
                        pq_oetf(c[0] * 100.0 / 10000.0),
                        pq_oetf(c[1] * 100.0 / 10000.0),
                        pq_oetf(c[2] * 100.0 / 10000.0),
                    )
                }
            };
            out.push(to_u16(r));
            out.push(to_u16(g));
            out.push(to_u16(b));
        }
        Ok(Some((w as u32, h as u32, out)))
    }

    fn to_u16(v: f32) -> u16 {
        (v.clamp(0.0, 1.0) * 65535.0).round() as u16
    }

    /// Inverse of [`pq_eotf`]: a share of 10000 nits → PQ code in [0,1] (SMPTE ST 2084).
    fn pq_oetf(l: f32) -> f32 {
        const M1: f32 = 0.159_301_76;
        const M2: f32 = 78.84375;
        const C1: f32 = 0.8359375;
        const C2: f32 = 18.851_562;
        const C3: f32 = 18.6875;
        let lm = l.clamp(0.0, 1.0).powf(M1);
        ((C1 + C2 * lm) / (1.0 + C3 * lm)).powf(M2)
    }

    /// Linear Rec.709 → linear Rec.2020 (the inverse of [`rec2020_to_rec709`]).
    fn rec709_to_rec2020(c: [f32; 3]) -> [f32; 3] {
        [
            0.6274 * c[0] + 0.3293 * c[1] + 0.0433 * c[2],
            0.0691 * c[0] + 0.9195 * c[1] + 0.0114 * c[2],
            0.0164 * c[0] + 0.0880 * c[1] + 0.8956 * c[2],
        ]
    }

    /// The decoded samples with their layout: shared by the SDR and HDR paths.
    fn decode_raw(jxr: &[u8]) -> Result<(usize, usize, Vec<u8>, usize, Kind), String> {
        let mut dec = ImageDecode::with_reader(Cursor::new(jxr))
            .map_err(|e| format!("jxr open (not a JPEG XR file?): {e:?}"))?;
        let (w, h) = dec.get_size().map_err(|e| format!("jxr size: {e:?}"))?;
        if w <= 0 || h <= 0 {
            return Err(format!("jxr bad size {w}x{h}"));
        }
        let (w, h) = (w as usize, h as usize);
        let format = dec
            .get_pixel_format()
            .map_err(|e| format!("jxr format: {e:?}"))?;
        let info = PixelInfo::from_format(format);
        let bpp = info.bits_per_pixel();
        if bpp == 0 || !bpp.is_multiple_of(8) {
            return Err(format!("unsupported jxr packing: {format:?} ({bpp} bpp)"));
        }
        let stride = w * (bpp / 8);
        let mut raw = vec![0u8; stride * h];
        dec.copy_all(&mut raw, stride)
            .map_err(|e| format!("jxr decode: {e:?}"))?;
        let kind = classify(format).ok_or_else(|| {
            format!(
                "unsupported jxr pixel format: {format:?}. \
                 Please report this so the converter can handle it."
            )
        })?;
        Ok((w, h, raw, bpp, kind))
    }

    /// Decode + colour-pipeline a JPEG XR into a packed RGB8 buffer.
    fn decode_to_rgb8(jxr: &[u8]) -> Result<(u32, u32, Vec<u8>), String> {
        let mut dec = ImageDecode::with_reader(Cursor::new(jxr))
            .map_err(|e| format!("jxr open (not a JPEG XR file?): {e:?}"))?;
        let (w, h) = dec.get_size().map_err(|e| format!("jxr size: {e:?}"))?;
        if w <= 0 || h <= 0 {
            return Err(format!("jxr bad size {w}x{h}"));
        }
        let (w, h) = (w as usize, h as usize);

        let format = dec
            .get_pixel_format()
            .map_err(|e| format!("jxr format: {e:?}"))?;
        let info = PixelInfo::from_format(format);
        let bpp = info.bits_per_pixel();
        if bpp == 0 || !bpp.is_multiple_of(8) {
            // Sub-byte / unusual packings (e.g. RGB555/565) aren't produced
            // by PS5 captures; reject rather than silently mangle.
            return Err(format!("unsupported jxr packing: {format:?} ({bpp} bpp)"));
        }
        let stride = w * (bpp / 8);
        let mut raw = vec![0u8; stride * h];
        dec.copy_all(&mut raw, stride)
            .map_err(|e| format!("jxr decode: {e:?}"))?;

        // Pull each pixel into linear-light, display-referred RGB in [0,1]
        // according to the source format, then sRGB-encode to 8-bit.
        let kind = classify(format).ok_or_else(|| {
            format!(
                "unsupported jxr pixel format: {format:?}. \
                 Please report this so the converter can handle it."
            )
        })?;

        let npix = w * h;
        // Linear (or already-display) RGB triples, depending on `kind`.
        let mut lin: Vec<[f32; 3]> = Vec::with_capacity(npix);
        let mut peak = 1.0f32; // white point for tone-mapping (>=1 so we never over-brighten SDR)

        for p in 0..npix {
            let off = p * (bpp / 8);
            let px = &raw[off..off + bpp / 8];
            let rgb = match kind {
                Kind::Sdr8 { bgr, channels } => {
                    // Already display-referred sRGB 8-bit — keep as-is.
                    let (r, g, b) = read_u8_rgb(px, bgr, channels);
                    // Mark via NaN-free passthrough: store as "already sRGB"
                    // by leaving values in [0,1] gamma space; tone-map skips
                    // SDR (peak stays 1.0 and Reinhard with Lw=1 is identity-ish),
                    // so instead we short-circuit SDR entirely below.
                    [r, g, b]
                }
                Kind::PqInt { bits, channels } => {
                    let (r, g, b) = read_int_rgb(px, bits, channels);
                    // code [0,1] → PQ EOTF → linear Rec.2020 (0..1, 1.0=10000 nits)
                    // → relative to 100-nit SDR white → Rec.709 primaries.
                    let r = pq_eotf(r) * 100.0;
                    let g = pq_eotf(g) * 100.0;
                    let b = pq_eotf(b) * 100.0;
                    rec2020_to_rec709([r, g, b])
                }
                Kind::ScRgbFloat { half, channels } => {
                    // scRGB: linear Rec.709 already, 1.0 == SDR white.
                    read_float_rgb(px, half, channels)
                }
            };
            if !matches!(kind, Kind::Sdr8 { .. }) {
                peak = peak.max(rgb[0]).max(rgb[1]).max(rgb[2]);
            }
            lin.push(rgb);
        }

        let mut out = vec![0u8; npix * 3];
        match kind {
            Kind::Sdr8 { .. } => {
                // Values are already sRGB-encoded display values in [0,1].
                for (i, rgb) in lin.iter().enumerate() {
                    out[i * 3] = to_u8(rgb[0]);
                    out[i * 3 + 1] = to_u8(rgb[1]);
                    out[i * 3 + 2] = to_u8(rgb[2]);
                }
            }
            _ => {
                // HDR: tone-map linear → SDR linear, then sRGB-encode.
                let lw = peak.max(1.0);
                for (i, rgb) in lin.iter().enumerate() {
                    out[i * 3] = to_u8(srgb_oetf(reinhard_ext(rgb[0].max(0.0), lw)));
                    out[i * 3 + 1] = to_u8(srgb_oetf(reinhard_ext(rgb[1].max(0.0), lw)));
                    out[i * 3 + 2] = to_u8(srgb_oetf(reinhard_ext(rgb[2].max(0.0), lw)));
                }
            }
        }
        Ok((w as u32, h as u32, out))
    }

    #[derive(Clone, Copy)]
    enum Kind {
        /// Display-referred sRGB, 8 bits/channel.
        Sdr8 { bgr: bool, channels: usize },
        /// HDR integer, PQ-encoded, Rec.2020 primaries.
        PqInt { bits: u32, channels: usize },
        /// HDR float/half, scRGB linear, Rec.709 primaries.
        ScRgbFloat { half: bool, channels: usize },
    }

    /// Map a decoded JPEG XR pixel format onto our three handling paths.
    /// Returns `None` for formats PS5 captures don't use (CMYK/YCC/gray/…),
    /// which surface as a clear "please report" error.
    fn classify(f: PixelFormat) -> Option<Kind> {
        use PixelFormat::*;
        Some(match f {
            // 8-bit SDR, already display-ready.
            PixelFormat24bppRGB
            | PixelFormat32bppRGB
            | PixelFormat32bppRGBA
            | PixelFormat32bppPRGBA => Kind::Sdr8 {
                bgr: false,
                channels: chans(f),
            },
            PixelFormat24bppBGR
            | PixelFormat32bppBGR
            | PixelFormat32bppBGRA
            | PixelFormat32bppPBGRA => Kind::Sdr8 {
                bgr: true,
                channels: chans(f),
            },

            // ≥10-bit integer → treated as PS5 HDR (Rec.2100 PQ).
            PixelFormat32bppRGB101010 => Kind::PqInt {
                bits: 10,
                channels: 3,
            },
            PixelFormat48bppRGB => Kind::PqInt {
                bits: 16,
                channels: 3,
            },
            PixelFormat64bppRGBA => Kind::PqInt {
                bits: 16,
                channels: 4,
            },

            // Float / half-float → scRGB linear (Windows/NVIDIA-style HDR;
            // handled too so a stray non-PS5 .jxr still converts sensibly).
            PixelFormat96bppRGBFloat | PixelFormat128bppRGBFloat => Kind::ScRgbFloat {
                half: false,
                channels: 3,
            },
            PixelFormat128bppRGBAFloat | PixelFormat128bppPRGBAFloat => Kind::ScRgbFloat {
                half: false,
                channels: 4,
            },
            PixelFormat48bppRGBHalf | PixelFormat64bppRGBHalf => Kind::ScRgbFloat {
                half: true,
                channels: 3,
            },
            PixelFormat64bppRGBAHalf => Kind::ScRgbFloat {
                half: true,
                channels: 4,
            },

            _ => return None,
        })
    }

    fn chans(f: PixelFormat) -> usize {
        PixelInfo::from_format(f).channels()
    }

    fn read_u8_rgb(px: &[u8], bgr: bool, channels: usize) -> (f32, f32, f32) {
        let _ = channels;
        let (r, g, b) = if bgr {
            (px[2], px[1], px[0])
        } else {
            (px[0], px[1], px[2])
        };
        (r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0)
    }

    fn read_int_rgb(px: &[u8], bits: u32, channels: usize) -> (f32, f32, f32) {
        match bits {
            10 => {
                // 32bppRGB101010: little-endian u32 with R in the HIGH 10
                // bits, G in the middle, B in the LOW 10 bits. This matches
                // jxrlib's own RGB101010_RGB48 converter (JXRGluePFC.c):
                //   r = (v >> 20) & 0x3FF; g = (v >> 10) & 0x3FF; b = v & 0x3FF
                // (An earlier version read these reversed, which swapped red
                // and blue — gold rendered as cyan, red as blue.)
                let v = u32::from_le_bytes([px[0], px[1], px[2], px[3]]);
                let r = ((v >> 20) & 0x3FF) as f32 / 1023.0;
                let g = ((v >> 10) & 0x3FF) as f32 / 1023.0;
                let b = (v & 0x3FF) as f32 / 1023.0;
                (r, g, b)
            }
            16 => {
                let rd = |i: usize| u16::from_le_bytes([px[i * 2], px[i * 2 + 1]]) as f32 / 65535.0;
                let _ = channels;
                (rd(0), rd(1), rd(2))
            }
            _ => (0.0, 0.0, 0.0),
        }
    }

    fn read_float_rgb(px: &[u8], half: bool, channels: usize) -> [f32; 3] {
        let _ = channels;
        if half {
            let rd = |i: usize| half_to_f32(u16::from_le_bytes([px[i * 2], px[i * 2 + 1]]));
            [rd(0), rd(1), rd(2)]
        } else {
            let rd = |i: usize| {
                f32::from_le_bytes([px[i * 4], px[i * 4 + 1], px[i * 4 + 2], px[i * 4 + 3]])
            };
            [rd(0), rd(1), rd(2)]
        }
    }

    /// IEEE-754 half (binary16) → f32. Small, dependency-free.
    fn half_to_f32(h: u16) -> f32 {
        let sign = (h >> 15) & 1;
        let exp = (h >> 10) & 0x1F;
        let mant = (h & 0x3FF) as u32;
        let f = match exp {
            0 => {
                if mant == 0 {
                    0u32
                } else {
                    // subnormal
                    let mut e = -1i32;
                    let mut m = mant;
                    loop {
                        e += 1;
                        m <<= 1;
                        if m & 0x400 != 0 {
                            break;
                        }
                    }
                    let exp32 = (127 - 15 - e) as u32;
                    (exp32 << 23) | ((m & 0x3FF) << 13)
                }
            }
            0x1F => 0xFFu32 << 23 | (mant << 13), // inf/nan
            _ => ((exp as u32 + (127 - 15)) << 23) | (mant << 13),
        };
        f32::from_bits((sign as u32) << 31 | f)
    }

    /// SMPTE ST.2084 (PQ) EOTF. Input: code value in [0,1]. Output: display
    /// luminance as a fraction of 10000 cd/m² (so 1.0 == 10000 nits).
    fn pq_eotf(n: f32) -> f32 {
        // SMPTE ST.2084 constants, as exact compile-time rationals.
        const M1: f32 = 2610.0 / 16384.0;
        const M2: f32 = 2523.0 / 4096.0 * 128.0;
        const C1: f32 = 3424.0 / 4096.0;
        const C2: f32 = 2413.0 / 4096.0 * 32.0;
        const C3: f32 = 2392.0 / 4096.0 * 32.0;
        let n = n.clamp(0.0, 1.0);
        let np = n.powf(1.0 / M2);
        let num = (np - C1).max(0.0);
        let den = C2 - C3 * np;
        if den <= 0.0 {
            return 0.0;
        }
        (num / den).powf(1.0 / M1)
    }

    /// Linear Rec.2020 → linear Rec.709 (Bradford-adapted D65, standard 3×3).
    fn rec2020_to_rec709(c: [f32; 3]) -> [f32; 3] {
        let (r, g, b) = (c[0], c[1], c[2]);
        [
            1.6605 * r - 0.5876 * g - 0.0728 * b,
            -0.1246 * r + 1.1329 * g - 0.0083 * b,
            -0.0182 * r - 0.1006 * g + 1.1187 * b,
        ]
    }

    /// Extended Reinhard tone-map: maps [0, white] → [0, 1], peak→1.0.
    fn reinhard_ext(c: f32, white: f32) -> f32 {
        let w2 = white * white;
        (c * (1.0 + c / w2)) / (1.0 + c)
    }

    /// Linear → sRGB display encoding (gamma).
    fn srgb_oetf(c: f32) -> f32 {
        let c = c.clamp(0.0, 1.0);
        if c <= 0.003_130_8 {
            12.92 * c
        } else {
            1.055 * c.powf(1.0 / 2.4) - 0.055
        }
    }

    fn to_u8(c: f32) -> u8 {
        (c.clamp(0.0, 1.0) * 255.0 + 0.5) as u8
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn pq_reference_points() {
            // PQ(0) == 0 nits; PQ(1.0) == 10000 nits (==1.0 fraction).
            assert!(pq_eotf(0.0).abs() < 1e-6);
            assert!((pq_eotf(1.0) - 1.0).abs() < 1e-3);
            // ~0.508 code ≈ 100 nits (0.01 fraction) — the SMPTE midpoint.
            let mid = pq_eotf(0.5081);
            assert!((mid - 0.01).abs() < 1e-3, "got {mid}");
        }

        #[test]
        fn srgb_roundtrip_endpoints() {
            assert_eq!(to_u8(srgb_oetf(0.0)), 0);
            assert_eq!(to_u8(srgb_oetf(1.0)), 255);
            // linear 0.5 → sRGB ~0.735 → ~188
            assert!((to_u8(srgb_oetf(0.5)) as i32 - 188).abs() <= 1);
        }

        #[test]
        fn reinhard_maps_peak_to_one() {
            // Lw is always the image peak, so inputs are confined to [0, Lw].
            // Over that range the curve is 0→0, peak→1, monotonic, and ≤1.
            let lw = 8.0;
            assert!(reinhard_ext(0.0, lw).abs() < 1e-6);
            assert!((reinhard_ext(lw, lw) - 1.0).abs() < 1e-6);
            let mut prev = -1.0;
            let mut x = 0.0;
            while x <= lw {
                let y = reinhard_ext(x, lw);
                assert!(y >= prev, "not monotonic at {x}");
                assert!(y <= 1.0 + 1e-6, "exceeded 1.0 at {x}: {y}");
                prev = y;
                x += lw / 64.0;
            }
        }

        #[test]
        fn rgb101010_channel_order_matches_jxrlib() {
            // Pack distinct 10-bit values the way jxrlib lays out RGB101010:
            // R in the high bits, G in the middle, B in the low bits. Using
            // distinct magnitudes (R largest, B smallest) so any R/B swap —
            // the gold-renders-as-cyan bug — fails the test.
            let (r10, g10, b10) = (900u32, 500u32, 100u32);
            let v = (r10 << 20) | (g10 << 10) | b10;
            let px = v.to_le_bytes();
            let (r, g, b) = read_int_rgb(&px, 10, 3);
            assert!((r - r10 as f32 / 1023.0).abs() < 1e-6, "R wrong: {r}");
            assert!((g - g10 as f32 / 1023.0).abs() < 1e-6, "G wrong: {g}");
            assert!((b - b10 as f32 / 1023.0).abs() < 1e-6, "B wrong: {b}");
            assert!(r > g && g > b, "channel order swapped: r={r} g={g} b={b}");
        }

        #[test]
        fn half_float_basic() {
            assert_eq!(half_to_f32(0x0000), 0.0);
            assert_eq!(half_to_f32(0x3C00), 1.0); // 1.0 in half
            assert_eq!(half_to_f32(0x4000), 2.0); // 2.0 in half
        }

        #[test]
        fn rec2020_white_stays_neutral() {
            // Equal-energy white in 2020 maps to ~equal-energy in 709.
            let w = rec2020_to_rec709([1.0, 1.0, 1.0]);
            for c in w {
                assert!((c - 1.0).abs() < 0.02, "channel {c} drifted");
            }
        }
    }
}

#[cfg(all(test, unix))]
#[cfg(not(any(target_os = "android", target_os = "ios")))]
mod deadline_tests {
    use super::run_with_deadline;
    use std::process::Command;
    use std::time::{Duration, Instant};

    fn sh(script: &str) -> Command {
        let mut c = Command::new("sh");
        c.args(["-c", script]);
        c
    }

    #[test]
    fn a_decoder_that_never_returns_is_killed_at_the_deadline() {
        // F21: a decoder looping on a corrupt capture held its thread for the life of the app.
        let started = Instant::now();
        let out = run_with_deadline(sh("sleep 30"), Duration::from_millis(200)).unwrap();
        assert!(out.is_none(), "the deadline passed");
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "killed, not waited for"
        );
    }

    #[test]
    fn a_finished_decoder_reports_its_status_and_its_error_text() {
        let (status, stderr) = run_with_deadline(
            sh("echo 'not a JPEG XR file' >&2; exit 3"),
            Duration::from_secs(30),
        )
        .unwrap()
        .unwrap();
        assert_eq!(status.code(), Some(3));
        assert!(stderr.contains("not a JPEG XR file"), "{stderr}");
        let (ok, _) = run_with_deadline(sh("exit 0"), Duration::from_secs(30))
            .unwrap()
            .unwrap();
        assert!(ok.success());
    }

    #[test]
    fn a_chatty_decoder_does_not_stall_on_a_full_pipe() {
        let (status, stderr) = run_with_deadline(
            sh("i=0; while [ $i -lt 20000 ]; do echo xxxxxxxxxxxxxxxx >&2; i=$((i+1)); done"),
            Duration::from_secs(30),
        )
        .unwrap()
        .unwrap();
        assert!(status.success());
        assert!(stderr.len() > 64 * 1024, "{}", stderr.len());
    }
}
