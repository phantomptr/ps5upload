use super::*;

/// An H.264 High-profile SPS (1280x720, level 3.1, as x264 writes it) and a PPS.
const SPS: &[u8] = &[
    0x67, 0x64, 0x00, 0x1f, 0xac, 0xd9, 0x40, 0x50, 0x05, 0xbb, 0x01, 0x10, 0x00, 0x00, 0x03, 0x00,
    0x10, 0x00, 0x00, 0x03, 0x03, 0xc0, 0xf1, 0x83, 0x19, 0x60,
];
const PPS: &[u8] = &[0x68, 0xeb, 0xe3, 0xcb, 0x22, 0xc0];

fn annexb(nals: &[&[u8]]) -> Vec<u8> {
    let mut v = Vec::new();
    for n in nals {
        v.extend_from_slice(&[0, 0, 0, 1]);
        v.extend_from_slice(n);
    }
    v
}

/// Writes bits, for building an HEVC SPS.
#[derive(Default)]
struct BitWriter {
    out: Vec<u8>,
    n: u32,
}

impl BitWriter {
    fn bit(&mut self, b: u32) {
        if self.n.is_multiple_of(8) {
            self.out.push(0);
        }
        if b != 0 {
            *self.out.last_mut().unwrap() |= 0x80 >> (self.n % 8);
        }
        self.n += 1;
    }
    fn bits(&mut self, v: u64, n: u32) {
        for i in (0..n).rev() {
            self.bit(v.checked_shr(i).map_or(0, |x| (x & 1) as u32));
        }
    }
    fn ue(&mut self, v: u32) {
        let x = v as u64 + 1;
        let len = 64 - x.leading_zeros();
        self.bits(0, len - 1);
        self.bits(x, len);
    }
}

/// An HEVC SPS: Main 10 (profile 2), level 5.1, 4:2:0, 10-bit, `sub_layers` temporal layers.
fn hevc_sps(sub_layers: u8) -> Vec<u8> {
    let mut w = BitWriter::default();
    w.bits(0x4201, 16); // NAL header: type 33
    w.bits(0, 4); // vps id
    w.bits((sub_layers - 1) as u64, 3);
    w.bit(1); // temporal id nesting
              // general PTL: space 0, tier 0, profile 2; compatibility (bit 2); constraints; level 153.
    w.bits(2, 8);
    w.bits(0x2000_0000, 32);
    w.bits(0x9000_0000_0000, 48);
    w.bits(153, 8);
    let max_sub = (sub_layers - 1) as usize;
    for _ in 0..max_sub {
        w.bit(1); // sub-layer profile present
        w.bit(1); // sub-layer level present
    }
    if max_sub > 0 {
        for _ in max_sub..8 {
            w.bits(0, 2);
        }
    }
    for _ in 0..max_sub {
        w.bits(0x1234, 88);
        w.bits(120, 8);
    }
    w.ue(0); // sps id
    w.ue(1); // 4:2:0
    w.ue(1920);
    w.ue(1080);
    w.bit(1); // conformance window
    for x in [0, 0, 0, 4] {
        w.ue(x);
    }
    w.ue(2); // 10-bit luma
    w.ue(2); // 10-bit chroma
             // The rest of a real SPS, then the stop bit and zeros to the byte's end.
    w.bits(0x5a5, 12);
    w.bit(1);
    while !w.n.is_multiple_of(8) {
        w.bit(0);
    }
    w.out
}

#[test]
fn nal_units_split_on_three_and_four_byte_start_codes() {
    let b = [
        0, 0, 0, 1, 0x09, 0xf0, 0, 0, 1, 0x65, 0xaa, 0, 0, 0, 0, 1, 0x41, 0xbb,
    ];
    assert_eq!(
        nal_units(&b),
        vec![&[0x09, 0xf0][..], &[0x65, 0xaa][..], &[0x41, 0xbb][..]]
    );
    assert!(nal_units(&[1, 2, 3]).is_empty());
}

#[test]
fn frames_are_length_prefixed_without_delimiter_or_parameter_sets() {
    let au = annexb(&[&[0x09, 0xf0], SPS, PPS, &[0x65, 1, 2, 3], &[0x65, 4]]);
    assert_eq!(
        frame_data(&au, false),
        [0, 0, 0, 4, 0x65, 1, 2, 3, 0, 0, 0, 2, 0x65, 4]
    );
    // HEVC: AUD (35), VPS/SPS/PPS (32-34) go; an IDR slice (19, header 0x26 0x01) stays.
    let au = annexb(&[&[0x46, 0x01, 0x50], &[0x40, 0x01, 7], &[0x26, 0x01, 9]]);
    assert_eq!(frame_data(&au, true), [0, 0, 0, 3, 0x26, 0x01, 9]);
}

#[test]
fn the_avc_record_carries_profile_level_and_the_high_profile_extension() {
    let c = video_config(&annexb(&[SPS, PPS]), false).unwrap();
    assert_eq!(&c[..6], &[1, 0x64, 0x00, 0x1f, 0xff, 0xe1]);
    assert_eq!(u16::from_be_bytes([c[6], c[7]]) as usize, SPS.len());
    assert_eq!(&c[8..8 + SPS.len()], SPS);
    let p = 8 + SPS.len();
    assert_eq!(c[p], 1);
    assert_eq!(&c[p + 3..p + 3 + PPS.len()], PPS);
    // 4:2:0, 8-bit, no extensions.
    assert_eq!(&c[p + 3 + PPS.len()..], &[0xfd, 0xf8, 0xf8, 0]);
    // Nothing to describe without a PPS.
    assert!(video_config(&annexb(&[SPS]), false).is_none());
}

#[test]
fn the_hevc_record_reads_the_sps_with_and_without_sub_layers() {
    for layers in [1u8, 3] {
        let sps = hevc_sps(layers);
        let vps = [0x40, 0x01, 0x0c];
        let pps = [0x44, 0x01, 0xc1];
        let c = video_config(&annexb(&[&vps, &sps, &pps]), true).unwrap();
        assert_eq!(c[0], 1);
        assert_eq!(
            &c[1..13],
            &sps[3..15],
            "general profile, tier and level as they are"
        );
        assert_eq!(c[1] & 0x1f, 2, "Main 10");
        assert_eq!(c[12], 153, "level 5.1");
        assert_eq!(c[16], 0xfd, "4:2:0");
        assert_eq!(c[17], 0xfa, "10-bit luma");
        assert_eq!(c[18], 0xfa, "10-bit chroma");
        assert_eq!(c[21], ((layers & 7) << 3) | 0x04 | 3);
        assert_eq!(c[22], 3, "VPS, SPS and PPS arrays");
        assert_eq!(c[23], 0x80 | 32);
        assert_eq!(&c[24..26], &[0, 1]);
        assert_eq!(&c[28..31], &vps);
        // The SPS array follows.
        assert_eq!(c[31], 0x80 | 33);
        let n = u16::from_be_bytes([c[34], c[35]]) as usize;
        assert_eq!(&c[36..36 + n], &sps[..]);
    }
}

#[test]
fn emulation_prevention_bytes_do_not_shift_the_sps_fields() {
    // Zero compatibility flags put 00 00 03 into the SPS as it is sent; the record must still
    // read the fields after it (chroma and bit depth) from the unescaped bits.
    let mut sps = hevc_sps(1);
    for b in &mut sps[4..8] {
        *b = 0;
    }
    let mut escaped = Vec::new();
    let mut zeros = 0;
    for &b in &sps {
        if zeros >= 2 && b <= 3 {
            escaped.push(3);
            zeros = 0;
        }
        zeros = if b == 0 { zeros + 1 } else { 0 };
        escaped.push(b);
    }
    assert!(escaped.len() > sps.len(), "the test SPS needs an escape");
    let c = hevc_config(&[&[0x40, 0x01]], &[&escaped], &[&[0x44, 0x01]]).unwrap();
    assert_eq!(&c[1..13], &sps[3..15]);
    assert_eq!(c[16], 0xfd);
    assert_eq!(c[17], 0xfa);
    assert_eq!(c[18], 0xfa);
}

#[test]
fn tag_headers() {
    assert_eq!(&video_config_tag(&[1, 2], false), &[0x17, 0, 0, 0, 0, 1, 2]);
    assert_eq!(&video_frame_tag(&[9], true, false), &[0x17, 1, 0, 0, 0, 9]);
    assert_eq!(&video_frame_tag(&[9], false, false), &[0x27, 1, 0, 0, 0, 9]);
    assert_eq!(
        &video_config_tag(&[1], true),
        &[0x90, b'h', b'v', b'c', b'1', 1]
    );
    assert_eq!(
        &video_frame_tag(&[9], true, true),
        &[0x93, b'h', b'v', b'c', b'1', 9]
    );
    assert_eq!(
        &video_frame_tag(&[9], false, true),
        &[0xa3, b'h', b'v', b'c', b'1', 9]
    );
    assert_eq!(&audio_config_tag(&[0x11, 0x90]), &[0xaf, 0, 0x11, 0x90]);
    assert_eq!(&audio_frame_tag(&[7]), &[0xaf, 1, 7]);
}

#[test]
fn aac_lc_config_for_the_consoles_sound() {
    // AAC-LC (2), 48 kHz (index 3), stereo.
    assert_eq!(aac_lc_config(48000, 2), [0x11, 0x90]);
    assert_eq!(aac_lc_config(44100, 1), [0x12, 0x08]);
}

#[test]
fn metadata_names_the_codecs() {
    let m = MetaInfo {
        width: 1920,
        height: 1080,
        fps: 60,
        video_kbps: 15000,
        hevc: true,
        audio_kbps: 160,
        channels: 2,
    };
    let v = amf::decode(&metadata(&m));
    assert_eq!(v[0].as_str(), Some("@setDataFrame"));
    assert_eq!(v[1].as_str(), Some("onMetaData"));
    assert_eq!(v[2].get("width").and_then(Amf::as_f64), Some(1920.0));
    assert_eq!(
        v[2].get("videocodecid").and_then(Amf::as_f64),
        Some(f64::from(u32::from_be_bytes(*b"hvc1")))
    );
    assert_eq!(v[2].get("audiocodecid").and_then(Amf::as_f64), Some(10.0));
}
