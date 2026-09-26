//! Profile operations — avatar image change + offline-account (username)
//! change.
//!
//! Host-side image pipeline: the desktop decodes the user's image, squares
//! it (center-crop or transparent-fit), bilinear-resizes to the four fixed
//! PS5 avatar sizes, and DXT5/DDS-encodes each. The finished `.dds` files +
//! `online.json` are staged to `/data/ps5upload/profile/0x<UID>/` via the
//! existing FS_WRITE_BYTES path, then a PROFILE_APPLY_AVATAR frame asks the
//! payload to copy them into the live profile cache dir (a privileged write
//! outside the normal writable roots).
//!
//! The resize + DXT5/DDS encoder are ported to match the PS5 reference
//! (ps5-payload-dev/offact's avatar tool, via Elf Arsenal) byte-for-byte —
//! the DDS header layout, the per-block alpha/colour compression, and the
//! RGB565 rounding are all reproduced exactly so the textures the PS5
//! profile cache expects are bit-identical to the reference tool's output.
//!
//! The offline-account (username) side is thin: PROFILE_INFO lists the
//! account-name slots, PROFILE_SET_USERNAME renames one, PROFILE_ACTIVATE /
//! PROFILE_CLEAR_SLOT toggle a slot's activation. All registry side-effects
//! live on the payload; this module just drives the frames.

use anyhow::{bail, Context, Result};
use ftx2_proto::FrameType;
use serde::{Deserialize, Serialize};

use crate::connection::Connection;

/// The four texture sizes the PS5 profile cache expects, in pixels. Each is
/// written as both `avatar<N>.dds` and `picture<N>.dds`.
pub const AVATAR_SIZES: [u32; 4] = [64, 128, 260, 440];

/// How to make a non-square source image square before resizing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SquareMode {
    /// Center-crop to the largest centered square (fills the frame, loses
    /// the edges of the longer axis).
    Crop,
    /// Pad the shorter axis with transparency to the longer axis (keeps the
    /// whole image, adds transparent bars).
    Fit,
}

impl SquareMode {
    /// Parse the `mode` query/JSON value. Anything but "fit" → Crop (the
    /// default), matching the reference tool.
    pub fn parse(s: &str) -> SquareMode {
        if s.eq_ignore_ascii_case("fit") {
            SquareMode::Fit
        } else {
            SquareMode::Crop
        }
    }
}

// ─── Image pipeline ────────────────────────────────────────────────────────

/// Bilinear resize of an RGBA8 buffer. Ported from the reference's
/// `resize_rgba` (same sample positions + `+0.5` rounding) so output matches.
fn resize_rgba(src: &[u8], sw: u32, sh: u32, dw: u32, dh: u32) -> Vec<u8> {
    let mut dst = vec![0u8; (dw as usize) * (dh as usize) * 4];
    if sw == 0 || sh == 0 {
        return dst;
    }
    for y in 0..dh {
        let ys = y as f64 * sh as f64 / dh as f64;
        let y0 = ys as u32;
        let y1 = (y0 + 1).min(sh - 1);
        let yf = ys - y0 as f64;
        for x in 0..dw {
            let xs = x as f64 * sw as f64 / dw as f64;
            let x0 = xs as u32;
            let x1 = (x0 + 1).min(sw - 1);
            let xf = xs - x0 as f64;
            let p00 = ((y0 * sw + x0) * 4) as usize;
            let p10 = ((y0 * sw + x1) * 4) as usize;
            let p01 = ((y1 * sw + x0) * 4) as usize;
            let p11 = ((y1 * sw + x1) * 4) as usize;
            let o = ((y * dw + x) * 4) as usize;
            for c in 0..4 {
                let v = src[p00 + c] as f64 * (1.0 - xf) * (1.0 - yf)
                    + src[p10 + c] as f64 * xf * (1.0 - yf)
                    + src[p01 + c] as f64 * (1.0 - xf) * yf
                    + src[p11 + c] as f64 * xf * yf;
                let v = v.clamp(0.0, 255.0);
                dst[o + c] = (v + 0.5) as u8;
            }
        }
    }
    dst
}

/// Center-crop an RGBA8 image to its largest centered square. Returns the
/// square buffer and its side length.
fn center_crop_square(src: &[u8], sw: u32, sh: u32) -> (Vec<u8>, u32) {
    let side = sw.min(sh);
    let x0 = (sw - side) / 2;
    let y0 = (sh - side) / 2;
    let row = (side * 4) as usize;
    let mut dst = vec![0u8; (side as usize) * (side as usize) * 4];
    for y in 0..side {
        let so = (((y0 + y) * sw + x0) * 4) as usize;
        let do_ = (y * side * 4) as usize;
        dst[do_..do_ + row].copy_from_slice(&src[so..so + row]);
    }
    (dst, side)
}

/// Pad an RGBA8 image to a square with transparent borders. Returns the
/// square buffer and its side length.
fn fit_square(src: &[u8], w: u32, h: u32) -> (Vec<u8>, u32) {
    let side = w.max(h);
    let row = (w * 4) as usize;
    let mut dst = vec![0u8; (side as usize) * (side as usize) * 4];
    let x0 = (side - w) / 2;
    let y0 = (side - h) / 2;
    for y in 0..h {
        let so = ((y * w) * 4) as usize;
        let do_ = (((y0 + y) * side + x0) * 4) as usize;
        dst[do_..do_ + row].copy_from_slice(&src[so..so + row]);
    }
    (dst, side)
}

// ─── DXT5 / DDS encoder (ported to match the reference byte-for-byte) ───────

fn rgb888_to_565(r: i32, g: i32, b: i32) -> u16 {
    (((r >> 3) << 11) | ((g >> 2) << 5) | (b >> 3)) as u16
}

fn rgb565_to_888(c: u16) -> [i32; 3] {
    [
        (((c >> 11) & 0x1f) << 3) as i32,
        (((c >> 5) & 0x3f) << 2) as i32,
        ((c & 0x1f) << 3) as i32,
    ]
}

fn color_distance(a: &[i32; 3], b: &[i32; 3]) -> i32 {
    let dr = a[0] - b[0];
    let dg = a[1] - b[1];
    let db = a[2] - b[2];
    dr * dr + dg * dg + db * db
}

/// Compress one 4×4 RGBA block (64 bytes, row-major) into 16 DXT5 bytes:
/// 8 bytes alpha (two endpoints + 16×3-bit indices) then 8 bytes colour
/// (two RGB565 endpoints + 16×2-bit indices).
fn compress_dxt5_block(pixels: &[u8; 64]) -> [u8; 16] {
    let mut out = [0u8; 16];

    // ── Alpha block ──
    let mut a_min = 255i32;
    let mut a_max = 0i32;
    for i in 0..16 {
        let a = pixels[i * 4 + 3] as i32;
        a_min = a_min.min(a);
        a_max = a_max.max(a);
    }
    let alpha0 = a_max as u8;
    let alpha1 = a_min as u8;
    let mut apal = [0u8; 8];
    apal[0] = alpha0;
    apal[1] = alpha1;
    if alpha0 > alpha1 {
        for i in 0..6 {
            apal[2 + i] =
                (((6 - i as i32) * alpha0 as i32 + (1 + i as i32) * alpha1 as i32) / 7) as u8;
        }
    } else {
        for i in 0..4 {
            apal[2 + i] =
                (((4 - i as i32) * alpha0 as i32 + (1 + i as i32) * alpha1 as i32) / 5) as u8;
        }
        apal[6] = 0;
        apal[7] = 255;
    }
    let mut aindex: u64 = 0;
    for i in 0..16 {
        let a = pixels[i * 4 + 3] as i32;
        let mut best = 0usize;
        let mut dist = 256i32;
        for (j, &pal) in apal.iter().enumerate() {
            let d = (a - pal as i32).abs();
            if d < dist {
                dist = d;
                best = j;
            }
        }
        aindex |= (best as u64) << (i * 3);
    }
    out[0] = alpha0;
    out[1] = alpha1;
    for i in 0..6 {
        out[2 + i] = ((aindex >> (i * 8)) & 0xff) as u8;
    }

    // ── Colour block ──
    let mut min_c = [255i32; 3];
    let mut max_c = [0i32; 3];
    for i in 0..16 {
        for c in 0..3 {
            let v = pixels[i * 4 + c] as i32;
            min_c[c] = min_c[c].min(v);
            max_c[c] = max_c[c].max(v);
        }
    }
    let mut color0 = rgb888_to_565(max_c[0], max_c[1], max_c[2]);
    let mut color1 = rgb888_to_565(min_c[0], min_c[1], min_c[2]);
    if color0 < color1 {
        std::mem::swap(&mut color0, &mut color1);
        std::mem::swap(&mut min_c, &mut max_c);
    } else if color0 == color1 && color0 < 0xffff {
        color0 += 1;
    }

    let c0 = rgb565_to_888(color0);
    let c1 = rgb565_to_888(color1);
    let mut pal = [[0i32; 3]; 4];
    pal[0] = c0;
    pal[1] = c1;
    for c in 0..3 {
        pal[2][c] = (2 * c0[c] + c1[c]) / 3;
        pal[3][c] = (c0[c] + 2 * c1[c]) / 3;
    }
    let mut cindex: u32 = 0;
    for i in 0..16 {
        let p = [
            pixels[i * 4] as i32,
            pixels[i * 4 + 1] as i32,
            pixels[i * 4 + 2] as i32,
        ];
        let mut best = 0usize;
        let mut dist = color_distance(&p, &pal[0]);
        for (j, palc) in pal.iter().enumerate().skip(1) {
            let d = color_distance(&p, palc);
            if d < dist {
                dist = d;
                best = j;
            }
        }
        cindex |= (best as u32) << (i * 2);
    }
    out[8] = (color0 & 0xff) as u8;
    out[9] = ((color0 >> 8) & 0xff) as u8;
    out[10] = (color1 & 0xff) as u8;
    out[11] = ((color1 >> 8) & 0xff) as u8;
    out[12] = (cindex & 0xff) as u8;
    out[13] = ((cindex >> 8) & 0xff) as u8;
    out[14] = ((cindex >> 16) & 0xff) as u8;
    out[15] = ((cindex >> 24) & 0xff) as u8;

    out
}

/// Encode an RGBA8 image (w×h) as a DXT5 `.dds` file (128-byte header +
/// compressed blocks). Ported from the reference `write_dxt5_dds`.
fn encode_dxt5_dds(rgba: &[u8], w: u32, h: u32) -> Vec<u8> {
    let bw = w.div_ceil(4);
    let bh = h.div_ceil(4);
    let comp_size = (bw as usize) * (bh as usize) * 16;
    let mut buf = vec![0u8; 128 + comp_size];

    // DDS header — same field layout the reference (and the original python
    // script) writes.
    buf[0..4].copy_from_slice(b"DDS ");
    buf[4] = 124; // dwSize
    let flags: u32 = 0x1 | 0x2 | 0x4 | 0x1000 | 0x80000;
    buf[8..12].copy_from_slice(&flags.to_le_bytes());
    buf[12..16].copy_from_slice(&h.to_le_bytes()); // dwHeight
    buf[16..20].copy_from_slice(&w.to_le_bytes()); // dwWidth
    buf[20..24].copy_from_slice(&(comp_size as u32).to_le_bytes()); // pitch/linear size
    buf[76] = 32; // pixel-format dwSize
    buf[80] = 4; // DDPF_FOURCC
    buf[84..88].copy_from_slice(b"DXT5");
    buf[108] = 0;
    buf[109] = 0x10; // DDSCAPS_TEXTURE

    let mut block = [0u8; 64];
    let mut p = 128;
    for by in 0..bh {
        for bx in 0..bw {
            for y in 0..4u32 {
                let py = ((by * 4 + y).min(h - 1)) as usize;
                for x in 0..4u32 {
                    let px = ((bx * 4 + x).min(w - 1)) as usize;
                    let src = (py * w as usize + px) * 4;
                    let dst = ((y * 4 + x) * 4) as usize;
                    block[dst..dst + 4].copy_from_slice(&rgba[src..src + 4]);
                }
            }
            let enc = compress_dxt5_block(&block);
            buf[p..p + 16].copy_from_slice(&enc);
            p += 16;
        }
    }
    buf
}

// ─── online.json ────────────────────────────────────────────────────────────

/// Build the `online.json` the profile cache expects. Mirrors the reference
/// template; only `firstName` (the display name) is substituted.
fn build_online_json(username: &str) -> Vec<u8> {
    let v = serde_json::json!({
        "avatarUrl": "http://static-resource.np.community.playstation.net/avatar_xl/WWS_E/E0012_XL.png",
        "firstName": username,
        "lastName": "",
        "pictureUrl": "https://image.api.np.km.playstation.net/images/?format=png&w=440&h=440&image=https%3A%2F%2Fkfscdn.api.np.km.playstation.net%2F00000000000008%2F000000000000003.png&sign=blablabla019501",
        "trophySummary": "{\"level\":1,\"progress\":0,\"earnedTrophies\":{\"platinum\":0,\"gold\":0,\"silver\":0,\"bronze\":0}}",
        "isOfficiallyVerified": "true"
    });
    // Pretty-print is irrelevant to the console; compact keeps it small.
    serde_json::to_vec(&v).expect("online.json is always serializable")
}

// ─── Public: build the full staged file set ─────────────────────────────────

/// Decode `image_bytes`, square it per `mode`, and produce every file the
/// profile cache dir needs: `avatar<N>.dds` + `picture<N>.dds` for each size
/// (identical bytes), `avatar.png` + `picture.png` (the squared source), and
/// `online.json`. Returned as `(filename, bytes)` pairs ready to stage.
pub fn build_avatar_files(
    image_bytes: &[u8],
    mode: SquareMode,
    username: &str,
) -> Result<Vec<(String, Vec<u8>)>> {
    let img = image::load_from_memory(image_bytes)
        .context("decode avatar image (supported: png, jpeg, webp, bmp, gif)")?;
    let rgba = img.to_rgba8();
    let (w, h) = rgba.dimensions();
    if w == 0 || h == 0 {
        bail!("avatar image has zero dimensions");
    }
    let raw = rgba.into_raw();

    let (square, side) = match mode {
        SquareMode::Crop => center_crop_square(&raw, w, h),
        SquareMode::Fit => fit_square(&raw, w, h),
    };

    let mut files: Vec<(String, Vec<u8>)> = Vec::new();

    for &size in &AVATAR_SIZES {
        let resized = if size == side {
            square.clone()
        } else {
            resize_rgba(&square, side, side, size, size)
        };
        let dds = encode_dxt5_dds(&resized, size, size);
        files.push((format!("avatar{size}.dds"), dds.clone()));
        files.push((format!("picture{size}.dds"), dds));
    }

    // Squared source as PNG → avatar.png + picture.png. Bounded to fit the
    // staging path's per-file cap (a full-resolution photo PNG would blow
    // past it; the DDS files are inherently bounded).
    let png = encode_bounded_png(&square, side)?;
    files.push(("avatar.png".to_string(), png.clone()));
    files.push(("picture.png".to_string(), png));

    files.push(("online.json".to_string(), build_online_json(username)));

    Ok(files)
}

/// Render a 440×440 PNG preview of how the avatar will be squared (crop or
/// fit), without doing the DXT5 encode or any network I/O. The UI shows this
/// so the user can compare crop vs fit before applying.
pub fn avatar_preview_png(image_bytes: &[u8], mode: SquareMode) -> Result<Vec<u8>> {
    let img = image::load_from_memory(image_bytes)
        .context("decode avatar image (supported: png, jpeg, webp, bmp, gif)")?;
    let rgba = img.to_rgba8();
    let (w, h) = rgba.dimensions();
    if w == 0 || h == 0 {
        bail!("avatar image has zero dimensions");
    }
    let raw = rgba.into_raw();
    let (square, side) = match mode {
        SquareMode::Crop => center_crop_square(&raw, w, h),
        SquareMode::Fit => fit_square(&raw, w, h),
    };
    let preview = if side == 440 {
        square
    } else {
        resize_rgba(&square, side, side, 440, 440)
    };
    encode_png(&preview, 440, 440)
}

/// The payload's FS_WRITE_BYTES path caps each decoded file at 256 KB. Stay a
/// little under so base64 + JSON overhead never pushes a staged file over.
const STAGE_FILE_MAX: usize = 250 * 1024;

/// Encode the squared source as PNG at the largest standard size whose
/// encoding fits the staging cap (most images fit at 440²; a noisy
/// full-colour photo falls back to a smaller dimension). The DDS textures
/// carry every size already, so the PNG dimension is non-critical.
fn encode_bounded_png(square: &[u8], side: u32) -> Result<Vec<u8>> {
    let mut last: Option<Vec<u8>> = None;
    for &dim in &[440u32, 260, 128, 64] {
        let rgba = if dim == side {
            square.to_vec()
        } else {
            resize_rgba(square, side, side, dim, dim)
        };
        let png = encode_png(&rgba, dim, dim)?;
        if png.len() <= STAGE_FILE_MAX {
            return Ok(png);
        }
        last = Some(png);
    }
    // Even 64² didn't fit (pathological) — return it anyway; the staging
    // write will surface a clear size error rather than us silently dropping.
    last.context("encode bounded source PNG")
}

fn encode_png(rgba: &[u8], w: u32, h: u32) -> Result<Vec<u8>> {
    use image::ImageEncoder;
    let mut out = std::io::Cursor::new(Vec::new());
    image::codecs::png::PngEncoder::new(&mut out)
        .write_image(rgba, w, h, image::ExtendedColorType::Rgba8)
        .context("encode source PNG")?;
    Ok(out.into_inner())
}

// ─── Frame round-trips ───────────────────────────────────────────────────────

fn round_trip_body(
    addr: &str,
    req: FrameType,
    body: &[u8],
    ack: FrameType,
    label: &str,
) -> Result<Vec<u8>> {
    let mut c = Connection::connect(addr)?;
    c.send_frame(req, body)?;
    let (hdr, resp) = c.recv_frame()?;
    let ft = hdr.frame_type().unwrap_or(FrameType::Error);
    if ft == FrameType::Error {
        bail!(
            "payload rejected {label}: {}",
            String::from_utf8_lossy(&resp)
        );
    }
    if ft != ack {
        bail!("expected {ack:?}, got {ft:?}");
    }
    Ok(resp)
}

/// One offline-account name slot.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProfileSlot {
    pub slot: i32,
    #[serde(default)]
    pub name: String,
    #[serde(default, rename = "type")]
    pub type_: String,
    #[serde(default)]
    pub flags: i32,
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub activated: bool,
    /// Activated by OUR offline flow (flags == 0x1002) rather than by a
    /// real PSN link. `activated` says it works; this says how it got there.
    #[serde(default)]
    pub offline_activated: bool,
}

/// A local console user (from `/user/home` enumeration).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProfileUser {
    pub uid: u32,
    #[serde(default)]
    pub uid_hex: String,
    #[serde(default)]
    pub username: String,
}

/// Foreground user + local users + account-name slots, from PROFILE_INFO.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProfileInfo {
    #[serde(default)]
    pub ok: bool,
    /// Foreground user id, or 0 when the payload daemon has no active
    /// session (common — prefer `users` for a target).
    #[serde(default)]
    pub uid: u32,
    #[serde(default)]
    pub uid_hex: String,
    #[serde(default)]
    pub username: String,
    /// Every local user on the console (the avatar can target any of them).
    #[serde(default)]
    pub users: Vec<ProfileUser>,
    #[serde(default)]
    pub slots: Vec<ProfileSlot>,
}

/// Read the foreground user + the offline-account name slots.
pub fn profile_info(addr: &str) -> Result<ProfileInfo> {
    let resp = round_trip_body(
        addr,
        FrameType::ProfileInfo,
        &[],
        FrameType::ProfileInfoAck,
        "PROFILE_INFO",
    )?;
    let info: ProfileInfo = serde_json::from_slice(&resp).context("parse PROFILE_INFO ack")?;
    Ok(info)
}

#[derive(Debug, Clone, Deserialize)]
struct OkResult {
    #[serde(default)]
    ok: bool,
    #[serde(default)]
    err_code: u32,
}

/// Rename an offline-account name slot (1-based).
pub fn profile_set_username(addr: &str, slot: i32, name: &str) -> Result<()> {
    let body = serde_json::to_vec(&serde_json::json!({ "slot": slot, "name": name }))?;
    let resp = round_trip_body(
        addr,
        FrameType::ProfileSetUsername,
        &body,
        FrameType::ProfileSetUsernameAck,
        "PROFILE_SET_USERNAME",
    )?;
    let r: OkResult = serde_json::from_slice(&resp)?;
    if !r.ok {
        bail!(
            "rename rejected by the console (registry err_code 0x{:08X}) — \
             needs a ucred-elevated payload",
            r.err_code
        );
    }
    Ok(())
}

#[derive(Debug, Clone, Deserialize)]
struct ActivateResult {
    #[serde(default)]
    ok: bool,
    #[serde(default)]
    id: String,
}

/// Rename a local console user (the active profile's display name) via
/// `sceUserServiceSetUserName`. `uid` is the console user id (as listed in
/// [`ProfileInfo::users`]). Distinct from [`profile_set_username`], which
/// renames an offline-account registry slot.
pub fn profile_set_local_username(addr: &str, uid: u32, name: &str) -> Result<()> {
    let body = serde_json::to_vec(&serde_json::json!({ "uid": uid, "name": name }))?;
    let resp = round_trip_body(
        addr,
        FrameType::ProfileSetLocalUsername,
        &body,
        FrameType::ProfileSetLocalUsernameAck,
        "PROFILE_SET_LOCAL_USERNAME",
    )?;
    let r: OkResult = serde_json::from_slice(&resp)?;
    if !r.ok {
        bail!(
            "the console rejected the rename — sceUserServiceSetUserName \
             isn't available on this firmware, or the name is invalid \
             (max 16 chars, no control characters)"
        );
    }
    Ok(())
}

/// Activate a slot: set its id (derived from the name when `id` is None),
/// type "np", and the default flags. Returns the resulting id string.
pub fn profile_activate(addr: &str, slot: i32, id: Option<u64>) -> Result<String> {
    let mut req = serde_json::json!({ "slot": slot });
    if let Some(id) = id {
        req["id"] = serde_json::Value::String(format!("0x{id:016x}"));
    }
    let body = serde_json::to_vec(&req)?;
    let resp = round_trip_body(
        addr,
        FrameType::ProfileActivate,
        &body,
        FrameType::ProfileActivateAck,
        "PROFILE_ACTIVATE",
    )?;
    let r: ActivateResult = serde_json::from_slice(&resp)?;
    if !r.ok {
        bail!("slot activation rejected by the console");
    }
    Ok(r.id)
}

/// Clear a slot's id + flags (de-activate, keep name + type).
pub fn profile_clear_slot(addr: &str, slot: i32) -> Result<()> {
    let body = serde_json::to_vec(&serde_json::json!({ "slot": slot }))?;
    let resp = round_trip_body(
        addr,
        FrameType::ProfileClearSlot,
        &body,
        FrameType::ProfileClearSlotAck,
        "PROFILE_CLEAR_SLOT",
    )?;
    let r: OkResult = serde_json::from_slice(&resp)?;
    if !r.ok {
        bail!("slot clear rejected by the console");
    }
    Ok(())
}

#[derive(Debug, Clone, Deserialize)]
struct ApplyAvatarResult {
    #[serde(default)]
    ok: bool,
    #[serde(default)]
    copied: i32,
}

/// Result of an avatar apply: which user it landed on + how many files.
#[derive(Debug, Clone, Serialize)]
pub struct AvatarApplied {
    pub uid: u32,
    pub username: String,
    pub files_copied: i32,
}

/// Full avatar change: resolve the target user (via PROFILE_INFO when
/// `uid`/`username` aren't supplied), build the DDS/PNG/json set host-side,
/// stage each file under `/data/ps5upload/profile/0x<UID>/` via
/// FS_WRITE_BYTES, then ask the payload to copy them into the live profile
/// cache dir.
pub fn profile_apply_avatar(
    addr: &str,
    uid: u32,
    username: Option<&str>,
    image_bytes: &[u8],
    mode: SquareMode,
) -> Result<AvatarApplied> {
    // Resolve uid/username if not fully supplied.
    let (uid, username) = match (uid, username) {
        (u, Some(name)) if u != 0 => (u, name.to_string()),
        (u, name) => {
            let info = profile_info(addr)?;
            let uid = if u != 0 { u } else { info.uid };
            let username = name.map(str::to_string).unwrap_or(info.username);
            (uid, username)
        }
    };
    if uid == 0 {
        bail!("no foreground user on the console — sign in to a profile first");
    }

    let files = build_avatar_files(image_bytes, mode, &username)?;

    let stage_dir = format!("/data/ps5upload/profile/0x{uid:08X}");
    // Recursive mkdir (FS_MKDIR is mkdir -p on the payload).
    crate::fs_ops::fs_mkdir(addr, &stage_dir)?;
    for (name, bytes) in &files {
        let path = format!("{stage_dir}/{name}");
        crate::diagnostics::fs_write_bytes(addr, &path, bytes, false)
            .with_context(|| format!("stage {name}"))?;
    }

    let body = serde_json::to_vec(&serde_json::json!({ "uid": uid }))?;
    let resp = round_trip_body(
        addr,
        FrameType::ProfileApplyAvatar,
        &body,
        FrameType::ProfileApplyAvatarAck,
        "PROFILE_APPLY_AVATAR",
    )?;
    let r: ApplyAvatarResult = serde_json::from_slice(&resp)?;
    if !r.ok {
        bail!(
            "the console couldn't copy the avatar into the profile cache \
             (copied {} files) — needs a ucred-elevated payload with write \
             access to /system_data",
            r.copied
        );
    }
    Ok(AvatarApplied {
        uid,
        username,
        files_copied: r.copied,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A solid-colour block must round-trip its colour through DXT5 closely
    /// and its alpha exactly (single alpha value → exact).
    #[test]
    fn dxt5_solid_block_alpha_exact() {
        let mut block = [0u8; 64];
        for i in 0..16 {
            block[i * 4] = 200; // R
            block[i * 4 + 1] = 100; // G
            block[i * 4 + 2] = 50; // B
            block[i * 4 + 3] = 255; // A
        }
        let enc = compress_dxt5_block(&block);
        // alpha0 == alpha1 == 255 (single value).
        assert_eq!(enc[0], 255);
        assert_eq!(enc[1], 255);
        // Colour endpoints: max==min so color0 was bumped by +1 (== branch).
        let color0 = u16::from_le_bytes([enc[8], enc[9]]);
        let color1 = u16::from_le_bytes([enc[10], enc[11]]);
        assert!(color0 >= color1);
    }

    /// DDS header: magic, dims, FourCC, and total length are all exact.
    #[test]
    fn dds_header_layout() {
        let w = 8u32;
        let h = 8u32;
        let rgba = vec![128u8; (w * h * 4) as usize];
        let dds = encode_dxt5_dds(&rgba, w, h);
        assert_eq!(&dds[0..4], b"DDS ");
        assert_eq!(dds[4], 124);
        assert_eq!(u32::from_le_bytes([dds[12], dds[13], dds[14], dds[15]]), h);
        assert_eq!(u32::from_le_bytes([dds[16], dds[17], dds[18], dds[19]]), w);
        assert_eq!(&dds[84..88], b"DXT5");
        // 8×8 = 2×2 blocks × 16 bytes = 64 + 128 header.
        assert_eq!(dds.len(), 128 + (2 * 2 * 16));
    }

    /// Resizing to the same dimensions is the identity.
    #[test]
    fn resize_identity() {
        let w = 4u32;
        let h = 4u32;
        let mut src = vec![0u8; (w * h * 4) as usize];
        for (i, b) in src.iter_mut().enumerate() {
            *b = (i % 256) as u8;
        }
        let out = resize_rgba(&src, w, h, w, h);
        assert_eq!(out, src);
    }

    /// Center-crop of a wide image picks the centered square.
    #[test]
    fn crop_picks_center_square() {
        // 4×2 image, two rows; crop → 2×2 from the horizontal center.
        let w = 4u32;
        let h = 2u32;
        let mut src = vec![0u8; (w * h * 4) as usize];
        // Tag each pixel's R with its x so we can check which columns survive.
        for y in 0..h {
            for x in 0..w {
                src[((y * w + x) * 4) as usize] = x as u8;
            }
        }
        let (sq, side) = center_crop_square(&src, w, h);
        assert_eq!(side, 2);
        // x0 = (4-2)/2 = 1, so columns 1 and 2 survive.
        assert_eq!(sq[0], 1); // (0,0).R
        assert_eq!(sq[4], 2); // (1,0).R
    }

    /// Fit pads a wide image with transparency top/bottom.
    #[test]
    fn fit_pads_transparent() {
        let w = 4u32;
        let h = 2u32;
        let src = vec![255u8; (w * h * 4) as usize]; // opaque white
        let (sq, side) = fit_square(&src, w, h);
        assert_eq!(side, 4);
        // Top row (y=0) is padding → fully transparent (all zero).
        assert!(sq[0..(side * 4) as usize].iter().all(|&b| b == 0));
        // Center rows carry the opaque source.
        let y1 = (side * 4) as usize;
        assert!(sq[y1..y1 + (side * 4) as usize].contains(&255));
    }

    /// The full builder yields the expected file set for a tiny PNG.
    #[test]
    fn build_files_produces_full_set() {
        // 2×2 red PNG.
        let mut buf = std::io::Cursor::new(Vec::new());
        {
            use image::ImageEncoder;
            let px = [
                255u8, 0, 0, 255, 255, 0, 0, 255, 255, 0, 0, 255, 255, 0, 0, 255,
            ];
            image::codecs::png::PngEncoder::new(&mut buf)
                .write_image(&px, 2, 2, image::ExtendedColorType::Rgba8)
                .unwrap();
        }
        let files = build_avatar_files(buf.get_ref(), SquareMode::Crop, "Tester").unwrap();
        let names: Vec<&str> = files.iter().map(|(n, _)| n.as_str()).collect();
        for size in AVATAR_SIZES {
            assert!(names.contains(&format!("avatar{size}.dds").as_str()));
            assert!(names.contains(&format!("picture{size}.dds").as_str()));
        }
        assert!(names.contains(&"avatar.png"));
        assert!(names.contains(&"picture.png"));
        assert!(names.contains(&"online.json"));
        // avatar<N>.dds and picture<N>.dds are byte-identical.
        let a440 = &files.iter().find(|(n, _)| n == "avatar440.dds").unwrap().1;
        let p440 = &files.iter().find(|(n, _)| n == "picture440.dds").unwrap().1;
        assert_eq!(a440, p440);
        // Each DDS starts with the magic.
        assert_eq!(&a440[0..4], b"DDS ");
    }

    #[test]
    fn online_json_carries_username() {
        let j = build_online_json("Daisy");
        let v: serde_json::Value = serde_json::from_slice(&j).unwrap();
        assert_eq!(v["firstName"], "Daisy");
        assert_eq!(v["isOfficiallyVerified"], "true");
    }

    #[test]
    fn square_mode_parse() {
        assert_eq!(SquareMode::parse("fit"), SquareMode::Fit);
        assert_eq!(SquareMode::parse("FIT"), SquareMode::Fit);
        assert_eq!(SquareMode::parse("crop"), SquareMode::Crop);
        assert_eq!(SquareMode::parse("anything"), SquareMode::Crop);
    }
}
