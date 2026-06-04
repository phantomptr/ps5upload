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
#[cfg(not(any(target_os = "android", target_os = "ios")))]
#[tauri::command]
pub async fn screenshot_convert(
    src_path: String,
    dst_path: String,
    delete_source: bool,
) -> Result<String, String> {
    tokio::task::spawn_blocking(move || {
        let bytes = std::fs::read(&src_path).map_err(|e| format!("read {src_path}: {e}"))?;
        let png = desktop::convert_jxr_to_png(&bytes)?;
        std::fs::write(&dst_path, png).map_err(|e| format!("write {dst_path}: {e}"))?;
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

/// Mobile fallback: the JPEG XR codec is a desktop-only dependency, so on
/// Android/iOS the command exists (the shared frontend can always call it)
/// but reports that conversion isn't available there.
#[cfg(any(target_os = "android", target_os = "ios"))]
#[tauri::command]
pub async fn screenshot_convert(
    _src_path: String,
    _dst_path: String,
    _delete_source: bool,
) -> Result<String, String> {
    Err("Screenshot conversion is available in the desktop app only.".into())
}

#[cfg(not(any(target_os = "android", target_os = "ios")))]
mod desktop {
    use image::{ImageEncoder, ImageError};
    use jpegxr::{ImageDecode, PixelFormat, PixelInfo};
    use std::io::Cursor;

    /// Decode a JPEG XR byte buffer and return PNG-encoded bytes (RGB8).
    pub fn convert_jxr_to_png(jxr: &[u8]) -> Result<Vec<u8>, String> {
        let (width, height, rgb8) = decode_to_rgb8(jxr)?;
        let mut out = Vec::new();
        image::codecs::png::PngEncoder::new(Cursor::new(&mut out))
            .write_image(&rgb8, width, height, image::ExtendedColorType::Rgb8)
            .map_err(|e: ImageError| format!("png encode: {e}"))?;
        Ok(out)
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

        let format = dec.get_pixel_format().map_err(|e| format!("jxr format: {e:?}"))?;
        let info = PixelInfo::from_format(format);
        let bpp = info.bits_per_pixel();
        if bpp == 0 || bpp % 8 != 0 {
            // Sub-byte / unusual packings (e.g. RGB555/565) aren't produced
            // by PS5 captures; reject rather than silently mangle.
            return Err(format!("unsupported jxr packing: {format:?} ({bpp} bpp)"));
        }
        let stride = w * (bpp / 8);
        let mut raw = vec![0u8; stride * h];
        dec.copy_all(&mut raw, stride).map_err(|e| format!("jxr decode: {e:?}"))?;

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
            PixelFormat24bppRGB | PixelFormat32bppRGB | PixelFormat32bppRGBA
            | PixelFormat32bppPRGBA => Kind::Sdr8 { bgr: false, channels: chans(f) },
            PixelFormat24bppBGR | PixelFormat32bppBGR | PixelFormat32bppBGRA
            | PixelFormat32bppPBGRA => Kind::Sdr8 { bgr: true, channels: chans(f) },

            // ≥10-bit integer → treated as PS5 HDR (Rec.2100 PQ).
            PixelFormat32bppRGB101010 => Kind::PqInt { bits: 10, channels: 3 },
            PixelFormat48bppRGB => Kind::PqInt { bits: 16, channels: 3 },
            PixelFormat64bppRGBA => Kind::PqInt { bits: 16, channels: 4 },

            // Float / half-float → scRGB linear (Windows/NVIDIA-style HDR;
            // handled too so a stray non-PS5 .jxr still converts sensibly).
            PixelFormat96bppRGBFloat | PixelFormat128bppRGBFloat => {
                Kind::ScRgbFloat { half: false, channels: 3 }
            }
            PixelFormat128bppRGBAFloat | PixelFormat128bppPRGBAFloat => {
                Kind::ScRgbFloat { half: false, channels: 4 }
            }
            PixelFormat48bppRGBHalf | PixelFormat64bppRGBHalf => {
                Kind::ScRgbFloat { half: true, channels: 3 }
            }
            PixelFormat64bppRGBAHalf => Kind::ScRgbFloat { half: true, channels: 4 },

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
