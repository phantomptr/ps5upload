//! A DualSense's haptics while Remote Play plays (USB only).
//!
//! In DualSense mode a PS5 game drives the pad's two voice-coil actuators with sound rather
//! than rumble levels: the engine passes on 10 ms frames of 3 kHz stereo s16le PCM (rp_stream
//! `StreamSink::haptics`), the page hands each to `pad_haptics_push`, and they are played on
//! the controller's own USB audio device, whose channels 3 and 4 are the left and right
//! actuators (1 and 2 are its speaker and headphones, left silent). As chiaki-ng does
//! (`StreamSession::PushHapticsFrame`): stereo → 4 channels, 3 kHz → 48 kHz.
//!
//! Over Bluetooth the pad has no audio device (its haptics travel in vendor reports nobody has
//! documented), and a Mac shows the USB device as stereo until it is set to 4 channels in
//! Audio MIDI Setup; then no stream opens here, `pad_haptics_push` answers false, and the page
//! turns the haptics into rumble instead (lib/dualSense.ts), as chiaki-ng does.

// The HID and audio sides are desktop-only; on a phone only the commands' answers remain.
#![cfg_attr(any(target_os = "android", target_os = "ios"), allow(dead_code))]

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::Mutex;

/// The rate the console sends haptics at, and the rate they are played at.
pub const IN_RATE: u32 = 3000;
pub const OUT_RATE: u32 = 48000;
const RATIO: usize = (OUT_RATE / IN_RATE) as usize;
/// Played frames kept ready after a gap before playing starts again (20 ms: the console's
/// frames come in bursts), and the most kept (anything older is dropped, so haptics never lag).
const PREFILL: usize = OUT_RATE as usize / 50;
const MAX_QUEUED: usize = OUT_RATE as usize / 12;

/// Names the controller's audio device goes by: "DualSense Wireless Controller" on macOS and
/// Windows, "Wireless Controller" (ALSA's card) on Linux (chiaki DUALSENSE_AUDIO_DEVICE_NEEDLE).
const NEEDLES: [&str; 2] = ["DualSense", "Wireless Controller"];

pub fn is_dualsense_audio(name: &str) -> bool {
    NEEDLES.iter().any(|n| name.contains(n))
}

/// 3 kHz stereo s16le → 48 kHz stereo f32 (-1..1), linear between samples, carried across
/// frames so a frame boundary is not a step.
#[derive(Debug, Default)]
pub struct Upsampler {
    last: [f32; 2],
}

impl Upsampler {
    pub fn push(&mut self, pcm: &[u8], out: &mut VecDeque<[f32; 2]>) {
        for s in pcm.chunks_exact(4) {
            let next = [
                f32::from(i16::from_le_bytes([s[0], s[1]])) / 32768.0,
                f32::from(i16::from_le_bytes([s[2], s[3]])) / 32768.0,
            ];
            for k in 1..=RATIO {
                let t = k as f32 / RATIO as f32;
                out.push_back([
                    self.last[0] + (next[0] - self.last[0]) * t,
                    self.last[1] + (next[1] - self.last[1]) * t,
                ]);
            }
            self.last = next;
        }
    }
}

/// What the audio callback plays from.
#[derive(Debug, Default)]
pub struct Queue {
    frames: VecDeque<[f32; 2]>,
    up: Upsampler,
    /// Waiting for PREFILL frames before playing (at the start, and after running dry).
    filling: bool,
}

impl Queue {
    fn new() -> Self {
        Queue {
            filling: true,
            ..Default::default()
        }
    }

    pub fn push(&mut self, pcm: &[u8]) {
        self.up.push(pcm, &mut self.frames);
        let over = self.frames.len().saturating_sub(MAX_QUEUED);
        self.frames.drain(..over);
    }

    /// Fill `out` (interleaved, `channels` per frame) with the haptics on channels 3 and 4.
    pub fn fill<T>(&mut self, out: &mut [T], channels: usize, conv: impl Fn(f32) -> T) {
        if self.filling && self.frames.len() >= PREFILL {
            self.filling = false;
        }
        for f in out.chunks_mut(channels) {
            let v = if self.filling {
                None
            } else {
                self.frames.pop_front()
            };
            if v.is_none() {
                self.filling = true;
            }
            let [l, r] = v.unwrap_or([0.0, 0.0]);
            for (c, s) in f.iter_mut().enumerate() {
                *s = conv(match c {
                    2 => l,
                    3 => r,
                    _ => 0.0,
                });
            }
        }
    }
}

static QUEUE: Mutex<Option<Queue>> = Mutex::new(None);
/// The controller's stream: closed (or it could not be opened), opening, or playing.
const CLOSED: u8 = 0;
const OPENING: u8 = 1;
const PLAYING: u8 = 2;
static STATE: AtomicU8 = AtomicU8::new(CLOSED);
/// The console's haptics strength setting (0 off, 1 strong, 2 medium, 3 weak).
static STRENGTH: AtomicU8 = AtomicU8::new(1);

pub fn set_strength(s: u8) {
    STRENGTH.store(s, Ordering::Relaxed);
}

#[cfg(not(any(target_os = "android", target_os = "ios")))]
mod output {
    use super::*;
    use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
    use std::sync::atomic::AtomicBool;
    use std::sync::Arc;
    use std::time::Duration;

    static RUNNING: Mutex<Option<Arc<AtomicBool>>> = Mutex::new(None);

    fn device_name(d: &cpal::Device) -> String {
        d.description()
            .map(|x| {
                let mut s = x.name().to_string();
                for e in x.extended() {
                    s.push(' ');
                    s.push_str(e);
                }
                s
            })
            .unwrap_or_default()
    }

    fn candidates() -> Vec<cpal::Device> {
        let host = cpal::default_host();
        match host.output_devices() {
            Ok(it) => it.filter(|d| is_dualsense_audio(&device_name(d))).collect(),
            Err(_) => Vec::new(),
        }
    }

    pub fn present() -> bool {
        !candidates().is_empty()
    }

    /// A 48 kHz config with 4 channels or more, in a sample format we write.
    fn config(d: &cpal::Device) -> Option<(cpal::StreamConfig, cpal::SampleFormat)> {
        let mut best: Option<cpal::SupportedStreamConfig> = None;
        for c in d.supported_output_configs().ok()? {
            if c.channels() < 4
                || !matches!(
                    c.sample_format(),
                    cpal::SampleFormat::F32 | cpal::SampleFormat::I16
                )
            {
                continue;
            }
            let Some(c) = c.try_with_sample_rate(OUT_RATE) else {
                continue;
            };
            // Fewest channels (4 when offered), f32 when both are.
            let better = best.as_ref().map_or(true, |b| {
                c.channels() < b.channels()
                    || (c.channels() == b.channels()
                        && c.sample_format() == cpal::SampleFormat::F32)
            });
            if better {
                best = Some(c);
            }
        }
        let c = best?;
        let format = c.sample_format();
        Some((c.into(), format))
    }

    fn build(d: &cpal::Device, stop: Arc<AtomicBool>) -> Option<cpal::Stream> {
        let (cfg, format) = config(d)?;
        let ch = usize::from(cfg.channels);
        let on_err = move |e: cpal::StreamError| {
            eprintln!("[pad-haptics] stream error: {e}");
            stop.store(true, Ordering::Relaxed);
        };
        fn cb<T: cpal::Sample + cpal::FromSample<f32>>(
            ch: usize,
        ) -> impl FnMut(&mut [T], &cpal::OutputCallbackInfo) + Send + 'static {
            move |out: &mut [T], _| {
                let mut g = QUEUE.lock().unwrap_or_else(|e| e.into_inner());
                match g.as_mut() {
                    Some(q) => q.fill(out, ch, T::from_sample),
                    None => out.fill(T::EQUILIBRIUM),
                }
            }
        }
        let s = match format {
            cpal::SampleFormat::F32 => {
                d.build_output_stream::<f32, _, _>(&cfg, cb::<f32>(ch), on_err, None)
            }
            _ => d.build_output_stream::<i16, _, _>(&cfg, cb::<i16>(ch), on_err, None),
        };
        match s {
            Ok(s) => s.play().ok().map(|_| s),
            Err(e) => {
                eprintln!("[pad-haptics] cannot open {}: {e}", device_name(d));
                None
            }
        }
    }

    fn run(stop: Arc<AtomicBool>) {
        let stream = candidates()
            .into_iter()
            .find_map(|d| build(&d, stop.clone()).map(|s| (s, device_name(&d))));
        let Some((stream, name)) = stream else {
            eprintln!("[pad-haptics] no DualSense audio device with 4 channels at 48 kHz");
            STATE.store(CLOSED, Ordering::Relaxed);
            return;
        };
        eprintln!("[pad-haptics] playing haptics on {name}");
        STATE.store(PLAYING, Ordering::Relaxed);
        while !stop.load(Ordering::Relaxed) {
            std::thread::sleep(Duration::from_millis(100));
        }
        STATE.store(CLOSED, Ordering::Relaxed);
        drop(stream);
    }

    pub fn set(on: bool) {
        let mut cur = RUNNING.lock().unwrap_or_else(|e| e.into_inner());
        if on {
            if cur.as_ref().is_some_and(|s| !s.load(Ordering::Relaxed)) {
                return;
            }
            *QUEUE.lock().unwrap_or_else(|e| e.into_inner()) = Some(Queue::new());
            STATE.store(OPENING, Ordering::Relaxed);
            let stop = Arc::new(AtomicBool::new(false));
            *cur = Some(stop.clone());
            let spawned = std::thread::Builder::new()
                .name("pad-haptics".into())
                .spawn(move || run(stop));
            if let Err(e) = spawned {
                eprintln!("[pad-haptics] could not start: {e}");
                STATE.store(CLOSED, Ordering::Relaxed);
                *cur = None;
            }
        } else {
            if let Some(stop) = cur.take() {
                stop.store(true, Ordering::Relaxed);
            }
            *QUEUE.lock().unwrap_or_else(|e| e.into_inner()) = None;
        }
    }
}

/// The controller's audio device is there (USB). Desktop only.
pub fn device_present() -> bool {
    #[cfg(not(any(target_os = "android", target_os = "ios")))]
    {
        output::present()
    }
    #[cfg(any(target_os = "android", target_os = "ios"))]
    {
        false
    }
}

/// Open (on) or close (off) the controller's audio device for haptics.
#[tauri::command]
pub fn pad_haptics_set(on: bool) -> bool {
    #[cfg(not(any(target_os = "android", target_os = "ios")))]
    {
        output::set(on);
        if !on {
            STRENGTH.store(1, Ordering::Relaxed);
        }
        true
    }
    #[cfg(any(target_os = "android", target_os = "ios"))]
    {
        let _ = on;
        false
    }
}

/// One 10 ms haptics frame (3 kHz stereo s16le). True when it is played on the pad (or the
/// pad's stream is still opening); false tells the page to make rumble of it instead.
#[tauri::command]
pub fn pad_haptics_push(pcm: Vec<u8>) -> bool {
    match STATE.load(Ordering::Relaxed) {
        PLAYING => {}
        OPENING => return true,
        _ => return false,
    }
    // The console's settings turned vibration off: nothing to play, and nothing to rumble.
    if STRENGTH.load(Ordering::Relaxed) == 0 {
        return true;
    }
    if let Some(q) = QUEUE.lock().unwrap_or_else(|e| e.into_inner()).as_mut() {
        q.push(&pcm);
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(pairs: &[(i16, i16)]) -> Vec<u8> {
        pairs
            .iter()
            .flat_map(|(l, r)| [l.to_le_bytes(), r.to_le_bytes()].concat())
            .collect()
    }

    #[test]
    fn upsamples_by_16_with_a_straight_line_between_samples() {
        let mut u = Upsampler::default();
        let mut out = VecDeque::new();
        u.push(&frame(&[(16384, -16384)]), &mut out);
        assert_eq!(out.len(), 16);
        // From silence up to 0.5 / -0.5: evenly, ending exactly there.
        assert!((out[7][0] - 0.25).abs() < 1e-6);
        assert!((out[15][0] - 0.5).abs() < 1e-6);
        assert!((out[15][1] + 0.5).abs() < 1e-6);
        // The next frame starts from where this one ended.
        out.clear();
        u.push(&frame(&[(16384, -16384)]), &mut out);
        assert!(out.iter().all(|f| (f[0] - 0.5).abs() < 1e-6));
        // A console frame: 30 stereo samples, 120 bytes → 480 played frames (10 ms).
        out.clear();
        u.push(&[0u8; 120], &mut out);
        assert_eq!(out.len(), 480);
    }

    #[test]
    fn haptics_go_to_channels_3_and_4_after_a_prefill() {
        let mut q = Queue::new();
        q.push(&frame(&[(32767, -32768); 30]));
        // 480 frames queued, under the 960-frame prefill: silence.
        let mut out = vec![1.0f32; 4 * 8];
        q.fill(&mut out, 4, |v| v);
        assert!(out.iter().all(|&s| s == 0.0));
        q.push(&frame(&[(32767, -32768); 30]));
        let mut out = vec![1.0f32; 4 * 960];
        q.fill(&mut out, 4, |v| v);
        let last = &out[4 * 959..];
        assert_eq!(last[0], 0.0);
        assert_eq!(last[1], 0.0);
        assert!((last[2] - 32767.0 / 32768.0).abs() < 1e-6);
        assert_eq!(last[3], -1.0);
        // Run dry: silence, and waiting for a prefill again.
        let mut out = vec![1i16; 4 * 4];
        q.fill(&mut out, 4, |v| (v * 32767.0) as i16);
        assert!(out.iter().all(|&s| s == 0));
    }

    #[test]
    fn the_queue_never_lags_more_than_its_cap() {
        let mut q = Queue::new();
        for _ in 0..50 {
            q.push(&[0u8; 120]);
        }
        assert_eq!(q.frames.len(), MAX_QUEUED);
    }

    #[test]
    fn finds_the_controller_by_name() {
        assert!(is_dualsense_audio("DualSense Wireless Controller"));
        assert!(is_dualsense_audio("Wireless Controller, USB Audio"));
        assert!(!is_dualsense_audio("MacBook Pro Speakers"));
    }
}
