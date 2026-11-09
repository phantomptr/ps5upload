//! FLV tag bodies, as RTMP messages carry them (type 8 audio, 9 video, 18 data): the console's
//! H.264 or HEVC as it came (only re-framed from Annex B start codes to length prefixes), AAC
//! from the device's own encoder, and onMetaData.
//!
//! * H.264: the classic FLV video tag (codec id 7) with an AVCDecoderConfigurationRecord as its
//!   sequence header.
//! * HEVC: enhanced RTMP (Veovera E-RTMP v1, what OBS 29.1+ and ffmpeg 6.1+ send): the ex-header
//!   bit, FourCC `hvc1`, an HEVCDecoderConfigurationRecord as the sequence start, frames as
//!   `CodedFramesX` (no composition offset: the console sends no B-frames). YouTube and custom
//!   servers that speak E-RTMP take it; Twitch, Kick and Facebook take H.264 only, which
//!   golive_dest.rs's codec check already enforces before a start.
//! * AAC: sound format 10, the AudioSpecificConfig as its sequence header, raw frames after.

use super::amf::{self, Amf};

pub(crate) const TAG_AUDIO: u8 = 8;
pub(crate) const TAG_VIDEO: u8 = 9;
pub(crate) const TAG_DATA: u8 = 18;

/// The NAL units of an Annex B buffer, without their start codes (trailing zero bytes, which
/// belong to the next start code, dropped).
pub(crate) fn nal_units(b: &[u8]) -> Vec<&[u8]> {
    let mut starts = Vec::new();
    let mut i = 0;
    while i + 3 <= b.len() {
        if b[i] == 0 && b[i + 1] == 0 && b[i + 2] == 1 {
            starts.push(i + 3);
            i += 3;
        } else {
            i += 1;
        }
    }
    starts
        .iter()
        .enumerate()
        .filter_map(|(n, &s)| {
            let mut e = starts.get(n + 1).map_or(b.len(), |&x| x - 3);
            while e > s && b[e - 1] == 0 {
                e -= 1;
            }
            (e > s).then(|| &b[s..e])
        })
        .collect()
}

fn nal_type(n: &[u8], hevc: bool) -> u8 {
    if hevc {
        (n[0] >> 1) & 0x3f
    } else {
        n[0] & 0x1f
    }
}

const H264_SPS: u8 = 7;
const H264_PPS: u8 = 8;
const H264_AUD: u8 = 9;
const HEVC_VPS: u8 = 32;
const HEVC_SPS: u8 = 33;
const HEVC_PPS: u8 = 34;
const HEVC_AUD: u8 = 35;

fn is_param(t: u8, hevc: bool) -> bool {
    if hevc {
        (HEVC_VPS..=HEVC_PPS).contains(&t)
    } else {
        t == H264_SPS || t == H264_PPS
    }
}

/// The parameter sets in `annexb` (the console's codec header, or a keyframe that carries its
/// own), as a decoder configuration record. None when a set is missing or unreadable.
pub(crate) fn video_config(annexb: &[u8], hevc: bool) -> Option<Vec<u8>> {
    let nals = nal_units(annexb);
    let of = |t: u8| -> Vec<&[u8]> {
        nals.iter()
            .copied()
            .filter(|n| nal_type(n, hevc) == t)
            .collect()
    };
    if hevc {
        hevc_config(&of(HEVC_VPS), &of(HEVC_SPS), &of(HEVC_PPS))
    } else {
        avc_config(&of(H264_SPS), &of(H264_PPS))
    }
}

/// One access unit as FLV carries it: each NAL unit behind its 4-byte length, without the
/// delimiter or the parameter sets (those go in the sequence header).
pub(crate) fn frame_data(au: &[u8], hevc: bool) -> Vec<u8> {
    let mut out = Vec::with_capacity(au.len() + 16);
    for n in nal_units(au) {
        let t = nal_type(n, hevc);
        let aud = if hevc { t == HEVC_AUD } else { t == H264_AUD };
        if aud || is_param(t, hevc) {
            continue;
        }
        out.extend_from_slice(&(n.len() as u32).to_be_bytes());
        out.extend_from_slice(n);
    }
    out
}

/// The NAL unit's payload with emulation-prevention bytes (00 00 03) taken out.
fn rbsp(n: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(n.len());
    let mut zeros = 0;
    for &b in n {
        if zeros >= 2 && b == 3 {
            zeros = 0;
            continue;
        }
        zeros = if b == 0 { zeros + 1 } else { 0 };
        out.push(b);
    }
    out
}

struct Bits<'a> {
    b: &'a [u8],
    at: usize,
}

impl Bits<'_> {
    fn bit(&mut self) -> Option<u32> {
        let byte = *self.b.get(self.at / 8)?;
        let v = (byte >> (7 - self.at % 8)) & 1;
        self.at += 1;
        Some(v as u32)
    }
    fn bits(&mut self, n: u32) -> Option<u64> {
        let mut v = 0u64;
        for _ in 0..n {
            v = (v << 1) | self.bit()? as u64;
        }
        Some(v)
    }
    fn skip(&mut self, n: usize) -> Option<()> {
        self.at += n;
        (self.at <= self.b.len() * 8).then_some(())
    }
    /// Exp-Golomb, unsigned.
    fn ue(&mut self) -> Option<u32> {
        let mut zeros = 0;
        while self.bit()? == 0 {
            zeros += 1;
            if zeros > 31 {
                return None;
            }
        }
        Some(((1u64 << zeros) - 1 + self.bits(zeros)?) as u32)
    }
}

/// AVCDecoderConfigurationRecord (ISO 14496-15 5.3.3.1), with the high-profile extension.
pub(crate) fn avc_config(sps: &[&[u8]], pps: &[&[u8]]) -> Option<Vec<u8>> {
    let first = sps.first()?;
    if first.len() < 4 || pps.is_empty() {
        return None;
    }
    let (profile, compat, level) = (first[1], first[2], first[3]);
    let mut v = vec![
        1,
        profile,
        compat,
        level,
        0xff,
        0xe0 | sps.len().min(31) as u8,
    ];
    for s in sps.iter().take(31) {
        v.extend_from_slice(&(s.len() as u16).to_be_bytes());
        v.extend_from_slice(s);
    }
    v.push(pps.len().min(255) as u8);
    for p in pps.iter().take(255) {
        v.extend_from_slice(&(p.len() as u16).to_be_bytes());
        v.extend_from_slice(p);
    }
    if matches!(profile, 100 | 110 | 122 | 144) {
        let r = rbsp(first);
        let mut b = Bits { b: &r, at: 32 };
        let mut ext = || -> Option<(u32, u32, u32)> {
            b.ue()?; // seq_parameter_set_id
            let chroma = b.ue()?;
            if chroma == 3 {
                b.bit()?;
            }
            Some((chroma, b.ue()?, b.ue()?))
        };
        let (chroma, luma8, chroma8) = ext().unwrap_or((1, 0, 0));
        v.push(0xfc | (chroma & 3) as u8);
        v.push(0xf8 | (luma8 & 7) as u8);
        v.push(0xf8 | (chroma8 & 7) as u8);
        v.push(0); // no SPS extensions
    }
    Some(v)
}

/// What an HEVC SPS says for the configuration record.
#[derive(Debug, PartialEq)]
struct HevcSps {
    /// profile_space(2) tier(1) profile_idc(5), compatibility flags (32), constraint flags (48),
    /// level: the 12 bytes of general_profile_tier_level as they are.
    ptl: [u8; 12],
    sub_layers: u8,
    temporal_id_nested: bool,
    chroma_format: u32,
    bit_depth_luma_minus8: u32,
    bit_depth_chroma_minus8: u32,
}

fn hevc_sps(sps: &[u8]) -> Option<HevcSps> {
    let r = rbsp(sps);
    // NAL header (2 bytes), then vps_id(4) max_sub_layers_minus1(3) temporal_id_nesting(1).
    let b0 = *r.get(2)?;
    let max_sub = (b0 >> 1) & 7;
    let nested = b0 & 1 == 1;
    let ptl: [u8; 12] = r.get(3..15)?.try_into().ok()?;
    let mut b = Bits { b: &r, at: 15 * 8 };
    let mut profile_present = [false; 8];
    let mut level_present = [false; 8];
    for i in 0..max_sub as usize {
        profile_present[i] = b.bit()? == 1;
        level_present[i] = b.bit()? == 1;
    }
    if max_sub > 0 {
        for _ in max_sub..8 {
            b.bits(2)?;
        }
    }
    for i in 0..max_sub as usize {
        if profile_present[i] {
            b.skip(88)?;
        }
        if level_present[i] {
            b.skip(8)?;
        }
    }
    b.ue()?; // sps_seq_parameter_set_id
    let chroma = b.ue()?;
    if chroma == 3 {
        b.bit()?;
    }
    b.ue()?; // width
    b.ue()?; // height
    if b.bit()? == 1 {
        for _ in 0..4 {
            b.ue()?;
        }
    }
    let luma8 = b.ue()?;
    let chroma8 = b.ue()?;
    Some(HevcSps {
        ptl,
        sub_layers: max_sub + 1,
        temporal_id_nested: nested,
        chroma_format: chroma,
        bit_depth_luma_minus8: luma8,
        bit_depth_chroma_minus8: chroma8,
    })
}

/// HEVCDecoderConfigurationRecord (ISO 14496-15 8.3.3.1.2).
pub(crate) fn hevc_config(vps: &[&[u8]], sps: &[&[u8]], pps: &[&[u8]]) -> Option<Vec<u8>> {
    if vps.is_empty() || pps.is_empty() {
        return None;
    }
    let s = hevc_sps(sps.first()?)?;
    let mut v = Vec::with_capacity(64);
    v.push(1);
    v.extend_from_slice(&s.ptl);
    v.extend_from_slice(&[0xf0, 0x00]); // min_spatial_segmentation_idc 0
    v.push(0xfc); // parallelismType 0 (unknown)
    v.push(0xfc | (s.chroma_format & 3) as u8);
    v.push(0xf8 | (s.bit_depth_luma_minus8 & 7) as u8);
    v.push(0xf8 | (s.bit_depth_chroma_minus8 & 7) as u8);
    v.extend_from_slice(&[0, 0]); // avgFrameRate unknown
                                  // constantFrameRate 0, numTemporalLayers, temporalIdNested, lengthSizeMinusOne 3.
    v.push(((s.sub_layers & 7) << 3) | ((s.temporal_id_nested as u8) << 2) | 3);
    v.push(3);
    for (t, list) in [(HEVC_VPS, vps), (HEVC_SPS, sps), (HEVC_PPS, pps)] {
        v.push(0x80 | t); // array_completeness 1
        v.extend_from_slice(&(list.len() as u16).to_be_bytes());
        for n in list {
            v.extend_from_slice(&(n.len() as u16).to_be_bytes());
            v.extend_from_slice(n);
        }
    }
    Some(v)
}

/// E-RTMP FourCC for HEVC.
const HVC1: &[u8; 4] = b"hvc1";
const EX_HEADER: u8 = 0x80;
const PACKET_SEQUENCE_START: u8 = 0;
const PACKET_CODED_FRAMES_X: u8 = 3;

/// The video sequence header tag.
pub(crate) fn video_config_tag(config: &[u8], hevc: bool) -> Vec<u8> {
    let mut v = Vec::with_capacity(config.len() + 5);
    if hevc {
        v.push(EX_HEADER | (1 << 4) | PACKET_SEQUENCE_START);
        v.extend_from_slice(HVC1);
    } else {
        v.extend_from_slice(&[0x17, 0x00, 0, 0, 0]);
    }
    v.extend_from_slice(config);
    v
}

/// A video frame tag (`data`: from `frame_data`).
pub(crate) fn video_frame_tag(data: &[u8], key: bool, hevc: bool) -> Vec<u8> {
    let frame_type: u8 = if key { 1 } else { 2 };
    let mut v = Vec::with_capacity(data.len() + 5);
    if hevc {
        v.push(EX_HEADER | (frame_type << 4) | PACKET_CODED_FRAMES_X);
        v.extend_from_slice(HVC1);
    } else {
        // AVC NALU, composition time 0.
        v.extend_from_slice(&[(frame_type << 4) | 7, 0x01, 0, 0, 0]);
    }
    v.extend_from_slice(data);
    v
}

/// AAC, 16-bit, stereo flags (FLV ignores rate and channels for AAC: the config says).
const AAC_HEADER: u8 = 0xaf;

pub(crate) fn audio_config_tag(asc: &[u8]) -> Vec<u8> {
    let mut v = vec![AAC_HEADER, 0x00];
    v.extend_from_slice(asc);
    v
}

pub(crate) fn audio_frame_tag(aac: &[u8]) -> Vec<u8> {
    let mut v = Vec::with_capacity(aac.len() + 2);
    v.extend_from_slice(&[AAC_HEADER, 0x01]);
    v.extend_from_slice(aac);
    v
}

/// AudioSpecificConfig for AAC-LC (ISO 14496-3 1.6.2.1).
pub(crate) fn aac_lc_config(rate: u32, channels: u8) -> [u8; 2] {
    const RATES: [u32; 13] = [
        96000, 88200, 64000, 48000, 44100, 32000, 24000, 22050, 16000, 12000, 11025, 8000, 7350,
    ];
    let idx = RATES.iter().position(|&r| r == rate).unwrap_or(3) as u16;
    let v: u16 = (2 << 11) | (idx << 7) | ((channels.clamp(1, 7) as u16) << 3);
    v.to_be_bytes()
}

/// What onMetaData says about the stream.
pub(crate) struct MetaInfo {
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub video_kbps: u32,
    pub hevc: bool,
    pub audio_kbps: u32,
    pub channels: u8,
}

/// `@setDataFrame onMetaData {…}`, the first thing a publisher sends.
pub(crate) fn metadata(m: &MetaInfo) -> Vec<u8> {
    let codec_id = if m.hevc {
        u32::from_be_bytes(*HVC1) as f64
    } else {
        7.0
    };
    let n = |x: u32| Amf::Number(x as f64);
    amf::encode(&[
        Amf::str("@setDataFrame"),
        Amf::str("onMetaData"),
        Amf::EcmaArray(vec![
            ("duration".into(), Amf::Number(0.0)),
            ("width".into(), n(m.width)),
            ("height".into(), n(m.height)),
            ("videodatarate".into(), n(m.video_kbps)),
            ("framerate".into(), n(m.fps)),
            ("videocodecid".into(), Amf::Number(codec_id)),
            ("audiodatarate".into(), n(m.audio_kbps)),
            ("audiosamplerate".into(), Amf::Number(48000.0)),
            ("audiosamplesize".into(), Amf::Number(16.0)),
            ("stereo".into(), Amf::Bool(m.channels >= 2)),
            ("audiocodecid".into(), Amf::Number(10.0)),
            ("encoder".into(), Amf::str("PS5Upload")),
            ("filesize".into(), Amf::Number(0.0)),
        ]),
    ])
}

#[cfg(test)]
#[path = "flv_tests.rs"]
mod tests;
