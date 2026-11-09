//! The sound converters on iPhone, iPad and Mac: AudioToolbox's AudioConverter, Opus → 16-bit
//! PCM (the system's Opus decoder, iOS 11 / macOS 10.13 and later) and PCM → AAC-LC.
//!
//! The framework is opened at run time (dlopen), as the app opens Photos
//! (client/src-tauri/src/commands/save_photos.rs): nothing changes in how the app or the engine
//! is linked, and a system without it only means Go live without ffmpeg isn't offered.
//!
//! AudioConverter pulls its input through a callback. Ours hands over what it has and then
//! answers NO_MORE (a status of our own), which ends that FillComplexBuffer call with what was
//! made and leaves the converter ready for more: the usual way to drive it as a stream.

use std::ffi::{c_char, c_int, c_void, CStr};
use std::sync::{Arc, OnceLock};

use super::audio::{AacEncoder, AudioCodecs, OpusDecoder, AAC_FRAME, RATE};

type OsStatus = i32;
type ConverterRef = *mut c_void;

const fn fcc(s: &[u8; 4]) -> u32 {
    u32::from_be_bytes(*s)
}

const FORMAT_LPCM: u32 = fcc(b"lpcm");
const FORMAT_AAC: u32 = fcc(b"aac ");
const FORMAT_OPUS: u32 = fcc(b"opus");
/// kAudioFormatFlagIsSignedInteger | kAudioFormatFlagIsPacked.
const LPCM_S16: u32 = 4 | 8;
/// kMPEG4Object_AAC_LC.
const AAC_LC: u32 = 2;
const PROP_BIT_RATE: u32 = fcc(b"brat");
const PROP_PRIME_INFO: u32 = fcc(b"prim");
const PROP_MAX_OUTPUT_PACKET: u32 = fcc(b"xops");
/// Our callback's "nothing more for now".
const NO_MORE: OsStatus = fcc(b"nomr") as OsStatus;
/// The longest Opus packet (120 ms) at 48 kHz.
const MAX_OPUS_FRAMES: u32 = 5760;

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct Asbd {
    sample_rate: f64,
    format_id: u32,
    format_flags: u32,
    bytes_per_packet: u32,
    frames_per_packet: u32,
    bytes_per_frame: u32,
    channels_per_frame: u32,
    bits_per_channel: u32,
    reserved: u32,
}

#[repr(C)]
struct AudioBuffer {
    channels: u32,
    size: u32,
    data: *mut c_void,
}

#[repr(C)]
struct AudioBufferList {
    count: u32,
    buffers: [AudioBuffer; 1],
}

#[repr(C)]
#[derive(Default)]
struct PacketDesc {
    start: i64,
    variable_frames: u32,
    size: u32,
}

#[repr(C)]
#[derive(Default)]
struct PrimeInfo {
    leading: u32,
    trailing: u32,
}

type InputProc = unsafe extern "C" fn(
    ConverterRef,
    *mut u32,
    *mut AudioBufferList,
    *mut *mut PacketDesc,
    *mut c_void,
) -> OsStatus;

type NewFn = unsafe extern "C" fn(*const Asbd, *const Asbd, *mut ConverterRef) -> OsStatus;
type DisposeFn = unsafe extern "C" fn(ConverterRef) -> OsStatus;
type SetFn = unsafe extern "C" fn(ConverterRef, u32, u32, *const c_void) -> OsStatus;
type GetFn = unsafe extern "C" fn(ConverterRef, u32, *mut u32, *mut c_void) -> OsStatus;
type FillFn = unsafe extern "C" fn(
    ConverterRef,
    InputProc,
    *mut c_void,
    *mut u32,
    *mut AudioBufferList,
    *mut PacketDesc,
) -> OsStatus;

struct Api {
    new: NewFn,
    dispose: DisposeFn,
    set: SetFn,
    get: GetFn,
    fill: FillFn,
}

extern "C" {
    fn dlopen(path: *const c_char, mode: c_int) -> *mut c_void;
    fn dlsym(handle: *mut c_void, name: *const c_char) -> *mut c_void;
}

const RTLD_LAZY: c_int = 1;

fn api() -> Option<&'static Api> {
    static API: OnceLock<Option<Api>> = OnceLock::new();
    API.get_or_init(|| unsafe {
        let h = dlopen(
            c"/System/Library/Frameworks/AudioToolbox.framework/AudioToolbox".as_ptr(),
            RTLD_LAZY,
        );
        if h.is_null() {
            return None;
        }
        let sym = |n: &CStr| {
            let p = dlsym(h, n.as_ptr());
            (!p.is_null()).then_some(p)
        };
        Some(Api {
            new: std::mem::transmute::<*mut c_void, NewFn>(sym(c"AudioConverterNew")?),
            dispose: std::mem::transmute::<*mut c_void, DisposeFn>(sym(c"AudioConverterDispose")?),
            set: std::mem::transmute::<*mut c_void, SetFn>(sym(c"AudioConverterSetProperty")?),
            get: std::mem::transmute::<*mut c_void, GetFn>(sym(c"AudioConverterGetProperty")?),
            fill: std::mem::transmute::<*mut c_void, FillFn>(sym(
                c"AudioConverterFillComplexBuffer",
            )?),
        })
    })
    .as_ref()
}

/// One AudioConverter, disposed with its owner.
struct Converter {
    api: &'static Api,
    r: ConverterRef,
}

// The converter is used from one task at a time (the run's), never shared.
unsafe impl Send for Converter {}

impl Drop for Converter {
    fn drop(&mut self) {
        unsafe {
            (self.api.dispose)(self.r);
        }
    }
}

impl Converter {
    fn new(from: &Asbd, to: &Asbd) -> Result<Self, String> {
        let api = api().ok_or("AudioToolbox is not available")?;
        let mut r: ConverterRef = std::ptr::null_mut();
        let st = unsafe { (api.new)(from, to, &mut r) };
        if st != 0 || r.is_null() {
            return Err(format!("AudioConverterNew: {}", status(st)));
        }
        Ok(Converter { api, r })
    }

    fn set_u32(&self, prop: u32, v: u32) -> OsStatus {
        unsafe { (self.api.set)(self.r, prop, 4, (&v as *const u32).cast()) }
    }

    fn get<T: Default>(&self, prop: u32) -> Option<T> {
        let mut v = T::default();
        let mut size = std::mem::size_of::<T>() as u32;
        let st = unsafe { (self.api.get)(self.r, prop, &mut size, (&mut v as *mut T).cast()) };
        (st == 0).then_some(v)
    }
}

/// An OSStatus as its four characters when it is one, else the number.
fn status(st: OsStatus) -> String {
    let b = (st as u32).to_be_bytes();
    if b.iter().all(|c| c.is_ascii_graphic() || *c == b' ') {
        format!("'{}'", String::from_utf8_lossy(&b))
    } else {
        st.to_string()
    }
}

fn pcm_format(channels: u32) -> Asbd {
    Asbd {
        sample_rate: RATE as f64,
        format_id: FORMAT_LPCM,
        format_flags: LPCM_S16,
        bytes_per_packet: 2 * channels,
        frames_per_packet: 1,
        bytes_per_frame: 2 * channels,
        channels_per_frame: channels,
        bits_per_channel: 16,
        reserved: 0,
    }
}

/// One Opus packet for the decoder's callback.
struct OpusFeed {
    data: *const u8,
    len: u32,
    channels: u32,
    given: bool,
    desc: PacketDesc,
}

unsafe extern "C" fn opus_input(
    _c: ConverterRef,
    packets: *mut u32,
    data: *mut AudioBufferList,
    desc: *mut *mut PacketDesc,
    user: *mut c_void,
) -> OsStatus {
    let f = &mut *(user as *mut OpusFeed);
    if f.given {
        *packets = 0;
        return NO_MORE;
    }
    f.given = true;
    (*data).count = 1;
    (*data).buffers[0] = AudioBuffer {
        channels: f.channels,
        size: f.len,
        data: f.data as *mut c_void,
    };
    *packets = 1;
    if !desc.is_null() {
        f.desc = PacketDesc {
            start: 0,
            variable_frames: 0,
            size: f.len,
        };
        *desc = &mut f.desc;
    }
    0
}

struct Decoder {
    conv: Converter,
    channels: u32,
    buf: Vec<i16>,
}

impl OpusDecoder for Decoder {
    fn decode(&mut self, packet: &[u8], out: &mut Vec<i16>) -> Result<(), String> {
        let ch = self.channels;
        self.buf.resize((MAX_OPUS_FRAMES * ch) as usize, 0);
        let mut feed = OpusFeed {
            data: packet.as_ptr(),
            len: packet.len() as u32,
            channels: ch,
            given: false,
            desc: PacketDesc::default(),
        };
        let mut frames = MAX_OPUS_FRAMES;
        let mut list = AudioBufferList {
            count: 1,
            buffers: [AudioBuffer {
                channels: ch,
                size: MAX_OPUS_FRAMES * ch * 2,
                data: self.buf.as_mut_ptr().cast(),
            }],
        };
        let st = unsafe {
            (self.conv.api.fill)(
                self.conv.r,
                opus_input,
                (&mut feed as *mut OpusFeed).cast(),
                &mut frames,
                &mut list,
                std::ptr::null_mut(),
            )
        };
        if st != 0 && st != NO_MORE {
            return Err(format!("decoding: {}", status(st)));
        }
        let n = (frames.min(MAX_OPUS_FRAMES) * ch) as usize;
        out.extend_from_slice(&self.buf[..n]);
        Ok(())
    }
}

/// PCM waiting for the encoder's callback.
struct PcmFeed {
    data: *const i16,
    /// Frames available, and taken so far.
    frames: u32,
    taken: u32,
    channels: u32,
}

unsafe extern "C" fn pcm_input(
    _c: ConverterRef,
    packets: *mut u32,
    data: *mut AudioBufferList,
    _desc: *mut *mut PacketDesc,
    user: *mut c_void,
) -> OsStatus {
    let f = &mut *(user as *mut PcmFeed);
    let n = (*packets).min(f.frames - f.taken);
    if n == 0 {
        *packets = 0;
        return NO_MORE;
    }
    (*data).count = 1;
    (*data).buffers[0] = AudioBuffer {
        channels: f.channels,
        size: n * f.channels * 2,
        data: f.data.add((f.taken * f.channels) as usize) as *mut c_void,
    };
    f.taken += n;
    *packets = n;
    0
}

struct Encoder {
    conv: Converter,
    channels: u32,
    pending: Vec<i16>,
    out: Vec<u8>,
    delay: u32,
}

impl AacEncoder for Encoder {
    fn encode(&mut self, pcm: &[i16], out: &mut Vec<Vec<u8>>) -> Result<(), String> {
        let ch = self.channels as usize;
        self.pending.extend_from_slice(pcm);
        loop {
            let mut feed = PcmFeed {
                data: self.pending.as_ptr(),
                frames: (self.pending.len() / ch) as u32,
                taken: 0,
                channels: self.channels,
            };
            let mut packets = 1u32;
            let mut desc = PacketDesc::default();
            let mut list = AudioBufferList {
                count: 1,
                buffers: [AudioBuffer {
                    channels: self.channels,
                    size: self.out.len() as u32,
                    data: self.out.as_mut_ptr().cast(),
                }],
            };
            let st = unsafe {
                (self.conv.api.fill)(
                    self.conv.r,
                    pcm_input,
                    (&mut feed as *mut PcmFeed).cast(),
                    &mut packets,
                    &mut list,
                    &mut desc,
                )
            };
            // What the callback handed over is the converter's now.
            self.pending.drain(..feed.taken as usize * ch);
            if st != 0 && st != NO_MORE {
                return Err(format!("encoding: {}", status(st)));
            }
            if packets == 0 {
                return Ok(());
            }
            let size = (list.buffers[0].size as usize).min(self.out.len());
            out.push(self.out[..size].to_vec());
        }
    }

    fn delay(&self) -> u32 {
        self.delay
    }
}

pub(crate) struct AudioToolbox;

impl AudioCodecs for AudioToolbox {
    fn name(&self) -> &'static str {
        "AudioToolbox"
    }

    fn open(
        &self,
        channels: u8,
        kbps: u32,
    ) -> Result<(Box<dyn OpusDecoder>, Box<dyn AacEncoder>), String> {
        let ch = channels.clamp(1, 2) as u32;
        let opus = Asbd {
            sample_rate: RATE as f64,
            format_id: FORMAT_OPUS,
            frames_per_packet: 480,
            channels_per_frame: ch,
            ..Asbd::default()
        };
        let dec = Converter::new(&opus, &pcm_format(ch)).map_err(|e| format!("Opus: {e}"))?;
        let aac = Asbd {
            sample_rate: RATE as f64,
            format_id: FORMAT_AAC,
            format_flags: AAC_LC,
            frames_per_packet: AAC_FRAME as u32,
            channels_per_frame: ch,
            ..Asbd::default()
        };
        let enc = Converter::new(&pcm_format(ch), &aac).map_err(|e| format!("AAC: {e}"))?;
        let bps = kbps.clamp(64, 320) * 1000;
        let st = enc.set_u32(PROP_BIT_RATE, bps);
        if st != 0 {
            crate::log_at!(
                Info,
                "golive",
                "golive: AAC at {} kbps refused ({}); the encoder's own rate instead",
                kbps,
                status(st)
            );
        }
        let max = enc
            .get::<u32>(PROP_MAX_OUTPUT_PACKET)
            .filter(|&m| m > 0)
            .unwrap_or(8192)
            .min(1 << 16);
        let delay = enc
            .get::<PrimeInfo>(PROP_PRIME_INFO)
            .map(|p| p.leading)
            .unwrap_or(0);
        Ok((
            Box::new(Decoder {
                conv: dec,
                channels: ch,
                buf: Vec::new(),
            }),
            Box::new(Encoder {
                conv: enc,
                channels: ch,
                pending: Vec::new(),
                out: vec![0u8; max as usize],
                delay,
            }),
        ))
    }
}

pub(crate) fn codecs() -> Option<Arc<dyn AudioCodecs>> {
    api()?;
    Some(Arc::new(AudioToolbox))
}

#[cfg(test)]
#[path = "apple_tests.rs"]
mod tests;
