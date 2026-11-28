# Microphone capture (PS5 Pro, FW 13.60, 2026-11-27)

Question: can PS5Upload Play capture the controller's microphone (or a headset on its jack) for
Play Rooms voice chat? **Yes, through libSceAudioIn, from the app as a normal sandboxed title.**
Capture works at 16 and 48 kHz, delivers samples at exactly real time, costs a fraction of a
percent of one core, and carries on while the app also plays sound.

What was and was not proven: nobody was at the console and its DualSense was off, so the
controller's own mic was never heard. A live signal was put into the console's voice input
through the Remote Play controller's microphone instead (the engine's experimental Remote Play
mic uplink, a 1 kHz tone from the Mac). The app captured that tone at the right level and rate.
The system picks the input device, so the DualSense's mic or a headset on it should arrive on
the same port. That last step still needs a check with someone holding the controller.

The test was a build of the app under a test title (`FAKE99268`) with `tests/mic_spike.cpp`
switched in. It logged to its own log file, and the install was removed afterwards. The capture
code the app can use is `src/ps5/mic.cpp` and `src/ps5/mic.hpp`.

## Calls

```c
int sceAudioInInit(void);                                   // 0 (0x8026000c/0x8026000e = already)
int sceAudioInOpen(int user, unsigned type, unsigned index, // handle 0x3000000N or error
                   unsigned grain, unsigned rate, unsigned param);
int sceAudioInHqOpen(int user, unsigned type, unsigned index,
                     unsigned grain, unsigned rate, unsigned param);
int sceAudioInInput(int handle, void *samples);             // blocks one grain; returns samples
int sceAudioInGetSilentState(int handle);                   // bits, 0 = live
int sceAudioInClose(int handle);
```

- **Linking:** libSceAudioIn is imported at link time. The SDK has no stub, so
  `tools/build-in-container.sh` makes one with `tools/make-stub.sh`, the same way as for
  libSceVideodec2. No `sceSysmoduleLoadModule` call is needed; the import resolved with nothing
  loaded by hand. Loading it at run time with `sceKernelLoadStartModule` and `sceKernelDlsym`
  does not work for an app, as public reports say (0x80020002 from `/system/common/lib`, no
  symbols from the sandbox path). The app's NEEDED entry is enough.
- **User:** `sceUserServiceGetInitialUser`. It was the same id as
  `sceUserServiceGetForegroundUser` here. User `0xff` (system) is refused with `0x809b0001`.
- **What opens** (`sceAudioInOpen`, index 0):

  | Port type | 16 kHz / 48 kHz, grain 128 or 256, mono or stereo | Grain 480, 512, 1024 |
  | --- | --- | --- |
  | 0 voice chat | opens | `0x80260102` |
  | 1 general | opens | `0x80260102` |
  | 5 voice recognition | opens (silent state 0 even while the others report muted; not captured from) | `0x80260102` |
  | 2, 3, 4, 6, 7 | `0x80260104` | `0x80260102` |

  `param`: 0 = S16 mono, 2 = S16 stereo.
- **sceAudioInHqOpen:** 16 kHz gives `0x80260103`, 48 kHz with grain 256 gives `0x80260102`,
  48 kHz mono with grain 128 gives `0x80260106`, and only **48 kHz stereo with grain 128** opens
  (types 0, 1, 5). The HQ port was not captured from.
- **Handles:** the first port is `0x30000007` every time. A second port can be open at the same
  time: voice chat at 48 kHz plus general at 16 kHz gave `0x30000007` and `0x30000006`.
  Opening takes 1.2 to 1.6 ms for voice chat and 0.2 ms for general.

## Measured

Each line below is from runs of 5 to 15 s (the open matrix and the timed captures), plus a
monitor port that stayed open for several minutes. Times come from the app's own log.

| | 16 kHz, grain 256 | 48 kHz, grain 256 |
| --- | --- | --- |
| One read blocks for | 16.0 ms (15.98 average, 16.09 worst) | 5.33 ms (5.47 worst) |
| Samples per wall-clock second | 100.0 to 100.1 % of the rate | 100.0 to 100.1 % of the rate |
| First read after open | 0.7 to 13 ms (less than one grain) | returns at once (0.6 to 3.8 ms) |
| First non-zero sample after open (live signal) | 1.4 to 14 ms | 2 to 4 ms |
| CPU (thread CPU time / wall time, including per-sample level maths) | 0.09 % of one core | 0.25 % of one core |
| 1 kHz tone put in at -24 dBFS, captured (voice chat port) | amplitude 9030 (RMS 6445) | amplitude 9470 (RMS 6710, peak 12163) |

- **Level:** a tone at -24 dBFS (amplitude 2048) came out at amplitude about 9470, so the path
  adds about +13 dB. The same tone at -12 dBFS came out at peak 30130, limited before clipping.
  Whether the Remote Play mic path or the console's mic processing adds the gain is unknown. A
  real controller mic will have its own gain.
- **Silent state:** bit 0x1 = no device, 0x4 = muted by a user setting.
  - 0x5 with no Remote Play session and the DualSense off: no device, muted.
  - 0x4 with a Remote Play session whose mic is off. The Remote Play controller counts as a
    device.
  - **0 with the Remote Play mic on.**
  - Each change showed up within one 5 s report, both on and off.
  - While muted or with no device, reads still return full grains of zeros at real time, with
    no errors.
- **Capturing while playing sound (the lockstep question):** capture ran at 100.0 to 100.1 %
  of real time in every case tried:
  - nothing else in the process playing sound (before the app's own sound started);
  - a tone on our own `sceAudioOut` port from its own thread;
  - the app's menu-music thread plus that tone;
  - **lockstep**: one 256-frame output block and one 256-frame capture grain per pass on the
    same thread. Each read then took 1.3 ms, because the output call does the waiting.

  The "capture falls to about 30 % of real time unless output and capture run in step" report
  (ps5-discord issue #11) was **not reproduced** from an app. That report came from code running
  inside SceShellUI, not from a title. The app can keep capture on its own thread and needs no
  lockstep.
- **Our output does not touch the captured signal:** while the remote mic carried a steady
  1 kHz tone, switching the app's own 1 kHz tone on the TV on and off changed the captured level
  by less than 0.3 %, at 16 and 48 kHz and on the voice chat and general ports. So nothing
  ducks or cancels the mic signal because of the app's own sound. Real echo from the TV into a
  controller mic, and whether the voice chat port cancels it, could not be tested: the Remote
  Play mic does not hear the room.
- **Gaps:** in about 4 minutes of capture with a live signal there were two short runs of zero
  grains: 19 grains (about 100 ms) once, and 1 to 4 grains once. They came while the Mac was
  sending the mic over Remote Play, so they are most likely gaps in that uplink, not capture
  drops. Each read still came at its normal time.
- **In the background:** holding PS for 1.3 s went to the home screen, with the app's tile
  playing (seen in an instant replay frame). **The app kept capturing the live signal at 100 %
  of real time with silent state 0** for over a minute, and carried on without a break when it
  came back to the front. The mic is not limited to the app in front, so the app must stop
  sending voice itself when it is not in front. The pad's 0x80000000 bit, which `padio` already
  reads, says when that is.
- **Rest mode** was not tested, because the console could go unreachable before the test
  install was removed. From phase 0: a game started in front closes this (game category) app,
  and its capture with it.

### Output port state (headset detection)

`sceAudioOutGetPortState` on our own 48 kHz port returned 0 with `output` 0x81, `channel` 1,
`volume` -1 and `rerouteCounter` 18. Bit 0x1 is the primary output (the TV). 0x80 appeared
while Remote Play was running, so it is probably the Remote Play or external output. The
headphone bit (0x40 in PS4 headers) was clear: no headset was connected. A headset on the
controller was not available, so its bit was not seen.

`sceAudioInGetHandleStatusInfo(handle, buffer)` returned 0, or 1 while the Remote Play mic was
on. The buffer had 1 at offset 0x10, 6 at 0x14 and 4 at 0x18 in both cases; what those mean is
unknown. `sceAudioInIsPadEmbeddedMic` (named in other projects) is not exported under that name
in the libSceAudioIn stub we have, so it was not called.

## For Play Rooms voice

- Open voice chat (type 0), mono, 16 kHz, grain 256, for the initial user, on a thread of its
  own. Read 16 ms grains into a ring, and encode 20 ms Opus frames (320 samples, VOIP
  application, 16 to 24 kb/s). Use 48 kHz with grain 256 if fullband matters. Both work, and
  capture costs nothing worth counting.
- Send nothing while `sceAudioInGetSilentState` is non-zero, and show "muted" or "no mic" from
  its bits. Send nothing while the app is not in front.
- The app's own game sound and voice playback can stay on the existing audio thread. Capture
  does not have to run in step with it.
- Echo: when the TV plays a guest's voice and the controller mic hears it, echo cancellation is
  unknown. Check it on the console with a person before relying on the console doing it. Opus
  has no echo cancellation, so plan a push-to-talk or mute fallback.

## Open questions

1. The DualSense's own mic, its mute button (expected to give 0x4), and a headset on its jack.
   This needs someone at the console with the controller on: speak, press mute, plug in a
   headset, and read the log.
2. Echo cancellation on the voice chat port, with the TV playing into a live controller mic.
3. What the gain is (about +13 dB here) with a real mic, and whether the general port is
   processed differently from voice chat. Both gave the same level from the Remote Play mic.
4. Rest mode with a port open: does capture resume after waking?
5. Type 5 (voice recognition) reported silent state 0 while types 0 and 1 reported muted. Does
   it ignore the mute? It was never read from.
6. The HQ port (48 kHz stereo, grain 128) was never read from.

## Reproducing

- Build the spike: `tests/mic_spike.cpp` explains how to switch it in.
- Install and launch it as a homebrew title: README "Install by hand".
- For a live signal with nobody at the console:
  - start Remote Play with `"mic": true` (`POST /api/ps5/rp/start`);
  - attach to it on `/api/ps5/rp/ws` with `{"host", "attach_only": true, "mic": true,
    "regist_key": "", "rp_key": ""}` (attaching uses the running session's keys);
  - send `{"mic": true}`, then `[5][Opus packet]` every 10 ms;
  - make the packets with `ffmpeg -f lavfi -i "sine=frequency=1000:sample_rate=48000:duration=60" -ac 2 -c:a libopus
    -b:a 32k -vbr off -frame_duration 10 -application lowdelay tone.ogg`, which gives the 40-byte
    packets the uplink takes.

## References (read only, nothing run)

- shadPS4, `src/core/libraries/audio/audioin.{h,cpp}` (GPL-2.0-or-later): argument order, port
  types, formats, silent bits.
- renan-marcel/PS5_PayloadSDK PR #1 and issue #2 (GPL-3.0-or-later): a homebrew title capturing
  at 48 kHz mono, grain 256. The same call from an elfldr payload fails with `0x809b0081`
  because it has no app identity.
- ThatRetiredDude/TokenStation5 PR #19 (GPL-3.0-or-later for its PS5 parts): `sceAudioInOpen(user,
  {0,1,5}, 0, 256, 16000, 0)` from a test title on 13.60, imported at link time after loading at
  run time failed.
- abhi245y/ps5-discord issue #11 (AGPL-3.0): the "capture at about 30 % unless in step" report
  (from SceShellUI, foreground user, type 0, 48 kHz, grain 256).
