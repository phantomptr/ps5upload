//! The sound converters on Android: MediaCodec through the NDK's C API (libmediandk, API 21+),
//! "audio/opus" → 16-bit PCM and PCM → "audio/mp4a-latm" (AAC-LC). Every Android phone has both
//! (the platform's own software codecs, c2.android.opus.decoder and c2.android.aac.encoder).
//!
//! libmediandk is opened at run time (dlopen; it is on the NDK's list of public libraries), so
//! the app's link line is untouched and a device without it only means Go live without ffmpeg
//! isn't offered.
//!
//! Both codecs run synchronously: queue an input buffer, then take every output that is ready
//! without waiting. The decoder needs the Opus header and its two delays as csd-0/1/2, as
//! Android's MediaExtractor would hand them over; the encoder's first output is its codec
//! config (flagged), which is skipped (flv.rs writes the AudioSpecificConfig).

use std::ffi::{c_char, c_int, c_long, c_void, CStr};
use std::sync::{Arc, OnceLock};

use super::audio::{AacEncoder, AudioCodecs, OpusDecoder, AAC_FRAME, OPUS_FRAME, RATE};

type MediaStatus = c_int;
type Codec = c_void;
type Format = c_void;

#[repr(C)]
#[derive(Default)]
struct BufferInfo {
    offset: i32,
    size: i32,
    presentation_time_us: i64,
    flags: u32,
}

const CONFIGURE_ENCODE: u32 = 1;
const FLAG_CODEC_CONFIG: u32 = 2;
const INFO_TRY_AGAIN_LATER: isize = -1;
const INFO_OUTPUT_FORMAT_CHANGED: isize = -2;
const INFO_OUTPUT_BUFFERS_CHANGED: isize = -3;
/// MediaCodecInfo.CodecProfileLevel.AACObjectLC.
const AAC_OBJECT_LC: i32 = 2;
/// How long to wait for an input buffer (µs).
const INPUT_WAIT_US: i64 = 20_000;

type CreateDecoderFn = unsafe extern "C" fn(*const c_char) -> *mut Codec;
type CreateEncoderFn = unsafe extern "C" fn(*const c_char) -> *mut Codec;
type ConfigureFn =
    unsafe extern "C" fn(*mut Codec, *const Format, *mut c_void, *mut c_void, u32) -> MediaStatus;
type StartFn = unsafe extern "C" fn(*mut Codec) -> MediaStatus;
type StopFn = unsafe extern "C" fn(*mut Codec) -> MediaStatus;
type DeleteFn = unsafe extern "C" fn(*mut Codec) -> MediaStatus;
type DequeueInputFn = unsafe extern "C" fn(*mut Codec, i64) -> isize;
type GetInputFn = unsafe extern "C" fn(*mut Codec, usize, *mut usize) -> *mut u8;
type QueueInputFn = unsafe extern "C" fn(*mut Codec, usize, c_long, usize, u64, u32) -> MediaStatus;
type DequeueOutputFn = unsafe extern "C" fn(*mut Codec, *mut BufferInfo, i64) -> isize;
type GetOutputFn = unsafe extern "C" fn(*mut Codec, usize, *mut usize) -> *mut u8;
type ReleaseOutputFn = unsafe extern "C" fn(*mut Codec, usize, bool) -> MediaStatus;
type FormatNewFn = unsafe extern "C" fn() -> *mut Format;
type FormatDeleteFn = unsafe extern "C" fn(*mut Format) -> MediaStatus;
type FormatSetI32Fn = unsafe extern "C" fn(*mut Format, *const c_char, i32);
type FormatSetStringFn = unsafe extern "C" fn(*mut Format, *const c_char, *const c_char);
type FormatSetBufferFn = unsafe extern "C" fn(*mut Format, *const c_char, *const c_void, usize);

struct Api {
    create_decoder: CreateDecoderFn,
    create_encoder: CreateEncoderFn,
    configure: ConfigureFn,
    start: StartFn,
    stop: StopFn,
    delete: DeleteFn,
    dequeue_input: DequeueInputFn,
    get_input: GetInputFn,
    queue_input: QueueInputFn,
    dequeue_output: DequeueOutputFn,
    get_output: GetOutputFn,
    release_output: ReleaseOutputFn,
    format_new: FormatNewFn,
    format_delete: FormatDeleteFn,
    format_set_i32: FormatSetI32Fn,
    format_set_string: FormatSetStringFn,
    format_set_buffer: FormatSetBufferFn,
}

extern "C" {
    fn dlopen(path: *const c_char, mode: c_int) -> *mut c_void;
    fn dlsym(handle: *mut c_void, name: *const c_char) -> *mut c_void;
}

const RTLD_LAZY: c_int = 1;

fn api() -> Option<&'static Api> {
    static API: OnceLock<Option<Api>> = OnceLock::new();
    API.get_or_init(|| unsafe {
        let h = dlopen(c"libmediandk.so".as_ptr(), RTLD_LAZY);
        if h.is_null() {
            return None;
        }
        let sym = |n: &CStr| {
            let p = dlsym(h, n.as_ptr());
            (!p.is_null()).then_some(p)
        };
        macro_rules! f {
            ($t:ty, $n:literal) => {
                std::mem::transmute::<*mut c_void, $t>(sym($n)?)
            };
        }
        Some(Api {
            create_decoder: f!(CreateDecoderFn, c"AMediaCodec_createDecoderByType"),
            create_encoder: f!(CreateEncoderFn, c"AMediaCodec_createEncoderByType"),
            configure: f!(ConfigureFn, c"AMediaCodec_configure"),
            start: f!(StartFn, c"AMediaCodec_start"),
            stop: f!(StopFn, c"AMediaCodec_stop"),
            delete: f!(DeleteFn, c"AMediaCodec_delete"),
            dequeue_input: f!(DequeueInputFn, c"AMediaCodec_dequeueInputBuffer"),
            get_input: f!(GetInputFn, c"AMediaCodec_getInputBuffer"),
            queue_input: f!(QueueInputFn, c"AMediaCodec_queueInputBuffer"),
            dequeue_output: f!(DequeueOutputFn, c"AMediaCodec_dequeueOutputBuffer"),
            get_output: f!(GetOutputFn, c"AMediaCodec_getOutputBuffer"),
            release_output: f!(ReleaseOutputFn, c"AMediaCodec_releaseOutputBuffer"),
            format_new: f!(FormatNewFn, c"AMediaFormat_new"),
            format_delete: f!(FormatDeleteFn, c"AMediaFormat_delete"),
            format_set_i32: f!(FormatSetI32Fn, c"AMediaFormat_setInt32"),
            format_set_string: f!(FormatSetStringFn, c"AMediaFormat_setString"),
            format_set_buffer: f!(FormatSetBufferFn, c"AMediaFormat_setBuffer"),
        })
    })
    .as_ref()
}

/// A started MediaCodec, stopped and deleted with its owner.
struct MediaCodec {
    api: &'static Api,
    c: *mut Codec,
    /// Presentation time of the next input (µs): the codec wants one, nothing reads it back.
    next_us: u64,
}

// Used from one task at a time (the run's), never shared.
unsafe impl Send for MediaCodec {}

impl Drop for MediaCodec {
    fn drop(&mut self) {
        unsafe {
            (self.api.stop)(self.c);
            (self.api.delete)(self.c);
        }
    }
}

/// An AMediaFormat being filled in, deleted after the codec is configured.
struct FormatBuilder {
    api: &'static Api,
    f: *mut Format,
}

impl Drop for FormatBuilder {
    fn drop(&mut self) {
        unsafe {
            (self.api.format_delete)(self.f);
        }
    }
}

impl FormatBuilder {
    fn new(api: &'static Api, mime: &CStr) -> Result<Self, String> {
        let f = unsafe { (api.format_new)() };
        if f.is_null() {
            return Err("AMediaFormat_new failed".into());
        }
        let b = FormatBuilder { api, f };
        unsafe { (api.format_set_string)(f, c"mime".as_ptr(), mime.as_ptr()) };
        Ok(b)
    }
    fn i32(&self, k: &CStr, v: i32) -> &Self {
        unsafe { (self.api.format_set_i32)(self.f, k.as_ptr(), v) };
        self
    }
    fn buffer(&self, k: &CStr, v: &[u8]) -> &Self {
        unsafe { (self.api.format_set_buffer)(self.f, k.as_ptr(), v.as_ptr().cast(), v.len()) };
        self
    }
}

impl MediaCodec {
    fn open(mime: &CStr, encoder: bool, fmt: &FormatBuilder) -> Result<Self, String> {
        let api = fmt.api;
        let c = unsafe {
            if encoder {
                (api.create_encoder)(mime.as_ptr())
            } else {
                (api.create_decoder)(mime.as_ptr())
            }
        };
        if c.is_null() {
            return Err(format!(
                "no {} for {}",
                if encoder { "encoder" } else { "decoder" },
                mime.to_string_lossy()
            ));
        }
        // Owned from here: dropped (stop + delete) on any error below.
        let m = MediaCodec { api, c, next_us: 0 };
        let flags = if encoder { CONFIGURE_ENCODE } else { 0 };
        let st =
            unsafe { (api.configure)(c, fmt.f, std::ptr::null_mut(), std::ptr::null_mut(), flags) };
        if st != 0 {
            return Err(format!("configure {}: {st}", mime.to_string_lossy()));
        }
        let st = unsafe { (api.start)(c) };
        if st != 0 {
            return Err(format!("start {}: {st}", mime.to_string_lossy()));
        }
        Ok(m)
    }

    /// Queues `data` as one input buffer (waits briefly for a free one).
    fn queue(
        &mut self,
        data: &[u8],
        frames: usize,
        out: &mut dyn FnMut(&[u8], u32),
    ) -> Result<(), String> {
        let api = self.api;
        let mut idx = unsafe { (api.dequeue_input)(self.c, INPUT_WAIT_US) };
        if idx < 0 {
            // Every input buffer is full: take the outputs and try once more.
            self.drain(out)?;
            idx = unsafe { (api.dequeue_input)(self.c, INPUT_WAIT_US) };
            if idx < 0 {
                return Err("the codec took no input".into());
            }
        }
        let mut cap = 0usize;
        let p = unsafe { (api.get_input)(self.c, idx as usize, &mut cap) };
        if p.is_null() || cap < data.len() {
            return Err(format!("input buffer of {cap} bytes for {}", data.len()));
        }
        unsafe { std::ptr::copy_nonoverlapping(data.as_ptr(), p, data.len()) };
        let st = unsafe { (api.queue_input)(self.c, idx as usize, 0, data.len(), self.next_us, 0) };
        if st != 0 {
            return Err(format!("queueInputBuffer: {st}"));
        }
        self.next_us += frames as u64 * 1_000_000 / RATE as u64;
        self.drain(out)
    }

    /// Every output ready now, as (bytes, flags).
    fn drain(&mut self, out: &mut dyn FnMut(&[u8], u32)) -> Result<(), String> {
        let api = self.api;
        loop {
            let mut info = BufferInfo::default();
            let idx = unsafe { (api.dequeue_output)(self.c, &mut info, 0) };
            match idx {
                INFO_TRY_AGAIN_LATER => return Ok(()),
                INFO_OUTPUT_FORMAT_CHANGED | INFO_OUTPUT_BUFFERS_CHANGED => continue,
                i if i < 0 => return Err(format!("dequeueOutputBuffer: {i}")),
                i => {
                    let mut cap = 0usize;
                    let p = unsafe { (api.get_output)(self.c, i as usize, &mut cap) };
                    let (off, size) = (info.offset.max(0) as usize, info.size.max(0) as usize);
                    if !p.is_null() && off + size <= cap && size > 0 {
                        let b = unsafe { std::slice::from_raw_parts(p.add(off), size) };
                        out(b, info.flags);
                    }
                    unsafe { (api.release_output)(self.c, i as usize, false) };
                }
            }
        }
    }
}

/// The Opus identification header (RFC 7845 5.1), mapping family 0.
fn opus_head(channels: u8) -> [u8; 19] {
    let mut h = [0u8; 19];
    h[..8].copy_from_slice(b"OpusHead");
    h[8] = 1;
    h[9] = channels;
    // pre-skip 0: the console's packets are a live stream with no encoder delay to trim.
    h[12..16].copy_from_slice(&RATE.to_le_bytes());
    h
}

struct Decoder {
    codec: MediaCodec,
}

impl OpusDecoder for Decoder {
    fn decode(&mut self, packet: &[u8], out: &mut Vec<i16>) -> Result<(), String> {
        let mut take = |b: &[u8], _flags: u32| {
            out.extend(b.as_chunks::<2>().0.iter().map(|s| i16::from_le_bytes(*s)));
        };
        self.codec.queue(packet, OPUS_FRAME, &mut take)
    }
}

struct Encoder {
    codec: MediaCodec,
    channels: usize,
    pending: Vec<u8>,
}

impl AacEncoder for Encoder {
    fn encode(&mut self, pcm: &[i16], out: &mut Vec<Vec<u8>>) -> Result<(), String> {
        for s in pcm {
            self.pending.extend_from_slice(&s.to_le_bytes());
        }
        let frame_bytes = AAC_FRAME * self.channels * 2;
        let mut take = |b: &[u8], flags: u32| {
            if flags & FLAG_CODEC_CONFIG == 0 {
                out.push(b.to_vec());
            }
        };
        // One AAC frame's worth per input buffer.
        while self.pending.len() >= frame_bytes {
            let chunk: Vec<u8> = self.pending.drain(..frame_bytes).collect();
            self.codec.queue(&chunk, AAC_FRAME, &mut take)?;
        }
        self.codec.drain(&mut take)
    }
}

pub(crate) struct MediaCodecs;

impl AudioCodecs for MediaCodecs {
    fn name(&self) -> &'static str {
        "MediaCodec"
    }

    fn open(
        &self,
        channels: u8,
        kbps: u32,
    ) -> Result<(Box<dyn OpusDecoder>, Box<dyn AacEncoder>), String> {
        let api = api().ok_or("libmediandk is not available")?;
        let ch = channels.clamp(1, 2);
        let opus = c"audio/opus";
        let f = FormatBuilder::new(api, opus)?;
        f.i32(c"sample-rate", RATE as i32)
            .i32(c"channel-count", ch as i32)
            .buffer(c"csd-0", &opus_head(ch))
            // Codec delay and seek pre-roll, in ns (64-bit, native order), as MediaExtractor gives them.
            .buffer(c"csd-1", &0u64.to_ne_bytes())
            .buffer(c"csd-2", &80_000_000u64.to_ne_bytes());
        let dec = MediaCodec::open(opus, false, &f)?;
        let aac = c"audio/mp4a-latm";
        let f = FormatBuilder::new(api, aac)?;
        f.i32(c"sample-rate", RATE as i32)
            .i32(c"channel-count", ch as i32)
            .i32(c"bitrate", (kbps.clamp(64, 320) * 1000) as i32)
            .i32(c"aac-profile", AAC_OBJECT_LC)
            .i32(c"max-input-size", (AAC_FRAME * ch as usize * 2 * 4) as i32);
        let enc = MediaCodec::open(aac, true, &f)?;
        Ok((
            Box::new(Decoder { codec: dec }),
            Box::new(Encoder {
                codec: enc,
                channels: ch as usize,
                pending: Vec::new(),
            }),
        ))
    }
}

pub(crate) fn codecs() -> Option<Arc<dyn AudioCodecs>> {
    api()?;
    Some(Arc::new(MediaCodecs))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_opus_header_is_rfc_7845s() {
        let h = opus_head(2);
        assert_eq!(&h[..8], b"OpusHead");
        assert_eq!((h[8], h[9]), (1, 2));
        assert_eq!(u32::from_le_bytes([h[12], h[13], h[14], h[15]]), 48_000);
        assert_eq!(h[18], 0, "mapping family 0");
    }
}
