# Phase 0 results (PS5 Pro, FW 13.60, 2026-11-27)

The spike app (`FAKE99271`) was installed as a homebrew folder, registered and launched through
the PS5Upload engine (helper 12.6.43), and watched through the log file and Remote Play captures.
It ran without elevation (sandboxed, as a normal app). All four parts work.

## Summary

| Part | Result |
| --- | --- |
| Hardware decode (libSceVideodec2) | Works for H.264 High and HEVC Main at 1080p60. Bit-exact: all 300 pictures of each test stream hash the same as ffmpeg's decode. |
| Display (sceVideoOut) | RGBA8 sRGB in the **tiled** layout works; linear is refused. Decoded NV12 is converted on the CPU: 2.9 ms per 1080p picture on 4 threads. Smooth 60 Hz playback confirmed on screen. |
| Controller (scePad) | Buttons, sticks and triggers read with the app in front. In the background the app keeps running and every sample has bit 0x80000000 set with buttons and sticks neutral. Rumble and light bar calls return 0. |
| Sound (sceAudioOut) | 48 kHz S16 stereo port opens and consumes 256-frame blocks at exactly real time. |
| Start / exit | Splash hidden, clean exit to the home screen with `sceSystemServiceLoadExec("exit")`. |
| Keep awake | `sceSystemServicePowerTick()` returns 0; awake and playing after 20 min with no input (not a controlled test). |
| Behind a game | Game category: closed when a game starts. Media category: suspended, resumes on return, sockets lost. Neither keeps running. |

## Decode

Setup as in the VLC port's ps5vdec.c: `sceSysmoduleLoadModule(207)` first (0, 7–20 ms), one compute
queue for the app's life (4.6 MiB direct memory), then per decoder a memory query, CPU memory
(flexible), GPU and CPU+GPU memory (direct memory type 12, CPU+GPU read/write), 16 picture buffers
and 8 access-unit buffers (4 MiB each) in direct memory. Decoders were created and deleted 6–7
times per run without trouble. Test streams: 5 s, 1080p60, 15 Mbit/s, no B-frames, keyframe every
60 pictures, an AUD before every picture.

| | H.264 depth 1 | H.264 depth 4 | HEVC depth 1 | HEVC depth 4 |
| --- | --- | --- | --- | --- |
| Decode call, average | 5.8 ms | 1.7 ms | 4.3 ms | 1.3 ms |
| Decode call, slowest | 16 ms | 17 ms | 7.2 ms | 7.3 ms |
| Throughput (decode only) | 173 pictures/s | 575 pictures/s | 234 pictures/s | 770 pictures/s |
| First picture out after access unit | 3 | 6 | **0** | 3 |
| CPU memory (flexible) | 44 MiB | 48 MiB | 26 MiB | 35 MiB |
| GPU memory | 110 MiB | 110 MiB | 201 MiB | 202 MiB |
| CPU+GPU memory | 11.6 MiB | 37 MiB | 28.5 MiB | 53 MiB |
| One picture buffer | 3.2 MiB | 3.2 MiB | 6.0 MiB | 6.0 MiB |
| Decoder setup | 8–40 ms | 13 ms | 12–40 ms | 14 ms |

- Pictures come out as NV12 (`frame_format` 0), pitch 2048 bytes. H.264 reports 1920x1088 (chroma
  starts at pitch x 1088), HEVC 1920x1080.
- **Latency:** HEVC at depth 1 gives the picture back from the same call that took its access unit.
  H.264 at depth 1 holds 3 pictures (50 ms at 60 Hz) even with no B-frames, probably because it
  waits for its picture-reorder buffer; worth checking with the console's own Remote Play stream
  (its VUI may say no reordering) or with a flush per access unit. Depth 4 adds 3 more pictures.
- Reading a decoded picture with the CPU is cheap (the memory is cached): hashing 3 MiB takes
  0.44 ms, copying it out 0.15 ms.

## Display

- `sceVideoOutOpen(0xff, 0, 0)` and `sceVideoOutSetFlipRate(0)` work.
- `0x8000000022000000` (RGBA8 sRGB): tiling mode 0 (PS5Term's tiled layout) registers;
  tiling mode 1 (linear) is refused with `0x80290007`.
- Probing other pixel-format codes was inconclusive: registering one more buffer set while set 1 was
  in use failed with `0x80290001` even for the known-good format. Showing decoder NV12 directly is
  not proven either way; the GPU (AGC) path is the way to avoid the CPU conversion.
- CPU NV12 to RGBA (plain C, BT.601, written into the tiled write-combined framebuffer, 1080p):
  1 thread 11.1 ms, 2 threads 5.7 ms, 4 threads 2.9 ms, 6 threads 2.1 ms. `clflush` of a 16 MiB
  framebuffer: 0.24 ms.
- Playback loop (one thread does decode, convert on 4 threads, pad overlay, flush, flip, wait for
  vblank): HEVC frame interval 16.68 ms average with no frame over 25 ms; H.264 16.8 ms average
  with 1–5 late frames per 5 s, from decode calls up to 16 ms. Pictures checked in Remote Play
  captures: correct colours, frame counter advancing, pad overlay live.

## Controller

- `sceUserServiceInitialize`, `sceUserServiceGetInitialUser`, `scePadInit`, `scePadOpen(user, 0, 0)`
  all succeed; the Remote Play controller shows up as this user's pad.
- Button bits seen: cross 0x4000, square 0x8000, triangle 0x1000, R1 0x0800, L2 0x0100, R2 0x0200.
  Sticks 0–255 (128 centre, right = 255, up = 0); triggers 0–255.
- `connected` is a **byte** at offset 0x4C: reading it as a 32-bit int gave values like 0xB0000001.
- With no input the pad returns about 5 new samples a second; a read every frame returns them all.
- Behind the control center and on the home screen the app keeps running (decode and flips continue
  at 60 Hz, decode about 15 % slower) and `scePadRead` keeps returning samples, every one with
  0x80000000 set and buttons, sticks and triggers neutral, so no input leaks to it. The bit clears
  as soon as the app is back in front.
- `scePadSetVibrationMode(2)` and `scePadSetVibration` return 0 and `scePadSetLightBar` returns 0.
  Nobody was at the console, so the rumble itself was not felt.

## Sound

`sceAudioOutInit` 0; `sceAudioOutOpen(0xff, main, 256 frames, 48 kHz, S16 stereo)` gives handle
`0x20000000`. The first three `sceAudioOutOutput` calls return at once (about 16 ms of queue), then
each call blocks until its block has played: 5.30 ms per 256-frame block over 2 s (5.33 expected).
So output latency is roughly 3–4 blocks (16–21 ms) plus the system mixer. Not heard (nobody at the
TV).

## Memory, launch and exit

- The app's malloc heap is small: a second 9 MiB `new[]` failed. Big buffers must come from
  `sceKernelMapFlexibleMemory`.
- Flexible memory available: 442 MiB by default; with `"kernel": {"flexibleMemorySize": 1073741824}`
  in param.json, 1018 MiB (direct memory size drops from 12288 to 11712 MiB).
- param.json `attribute` 0x62000000 and `attribute3` 0x80040 (from other streaming apps): the app
  launches with or without them; no difference seen in these tests.
- The very first launch (right after the first registration, a minute after waking the console)
  returned `0x80940010` and the title did not start; the helper's ShellUI fallback was dispatched
  and did not start it either. Every later launch worked, with each param.json variant (all fields,
  plain, `attribute` only, `attribute3` only, `kernel` only). Cause not established: either the
  title was not ready right after installing it, or kstuff was paused just then (an earlier note
  ties `0x80940010` to launching while ShadowMount+ had paused kstuff).
- `sceSystemServiceLoadExec("exit", NULL)` returns to the home screen; the process is gone.
- Calling `sceShellCoreUtilGetAutoPowerDownRemainingSeconds` from the app ends it at once (not
  available to an app).
- An overflow of my own (copying 2048-pitch pictures into a 1920-pitch buffer on the heap) made the
  third decoder fail with `0x811D0111` and the app die; fixed. Busy-spinning conversion threads were
  replaced with a condition variable so the decoder's own thread is never starved.

## Keep awake

The app ran in front for 20 minutes with no input at all (Remote Play stopped, nothing pressed),
calling `sceSystemServicePowerTick()` every 10 s (always 0). The console stayed awake (DDP 200) and
the app kept playing at 60 Hz the whole time; the first Remote Play picture afterwards was not
dimmed. This is not a controlled test: the console's own rest timer setting is unknown, and the
console may not rest while a game-category app is in front anyway. A run without the tick, past
the rest timer, would settle it.

## In the background: game category vs media category

Owner question: can the app keep running while a game is in front, like media apps? Tested with
the same build as two titles: `FAKE99271` (`applicationCategoryType` 0, game) and `FAKE99272`
(65536 + `contentBadgeType` 2, media, no `gameIntent`). Each logged a heartbeat every second from
its own thread, kept a TCP connection to a test server on the Mac (one ping a second, echoed),
played a quiet tone on its own audio thread, and kept decoding and flipping. Then a normal
installed game (LEGO Harry Potter Collection) was started in front.

| | Game category (0) | Media category (65536) |
| --- | --- | --- |
| Home screen / control center in front | keeps running: heartbeats, 60 Hz flips, tone and socket all continue; pad samples flagged 0x80000000 | same (checked by heartbeats; Remote Play shows nothing of a media app, see below) |
| A game started in front | **closed** within about a second: no last log line, socket closed, process gone, no prompt | **suspended**: heartbeats, flips and the tone stop at once; the process stays (resident memory shrank to 0.4 MiB) |
| 60 s later, back to the app | (gone; start it again) | resumes where it stopped: the next heartbeat says 148.9 s since the last one, decode, flips and the tone carry on |
| TCP connection | closed with the app | the Mac side saw a FIN about 16 s into the suspension; after resuming, the app's own `recv` never returned. Reconnect after every resume. |
| Audio while the game is in front | none (app closed) | none (suspended) |

Other media-category differences measured in the same runs:

- `bind()` is refused with `EACCES` (errno 13): a media app cannot listen on a port. Connecting
  out works.
- Remote Play captures of a media app are entirely black, including the system's control center on
  top of it (content protection); the game-category app shows normally.
- Direct memory 3 GiB instead of 12 GiB (flexible memory the same, 1018 MiB with the param.json
  field).
- About 25–45 % slower: H.264 decode call 8.5 ms instead of 5.8 ms, HEVC 5.6 instead of 4.3,
  NV12 conversion 13.4 ms instead of 11.1 ms on one thread.

So neither category keeps running behind a game. A media app survives the game and picks up where
it left off when the user goes back to it; a game-category app is closed. For the guest role that
makes little difference (the app is what's in front). For a host-side companion that should keep
talking to PS5Upload while a game runs, the app can't do it; the helper (a payload, not an app)
has to. When a game was started over the media app, `coredump.elf` ran for a few seconds; which
process it dumped is unknown (both the game and the suspended app were still there afterwards).
Launching the game over the media app was done once through the engine's launch API (it worked);
over the game-category app, from the home screen.

## Recommended architecture for phase 1

- **Threads:** network receive, then a decode thread (Videodec2, one decoder kept for the session),
  a present thread (convert + flip, paced by vblank, triple-buffered tiled RGBA), a pad thread
  (`scePadRead` every 4 ms, send state; neutral while bit 0x80000000 is set), and an audio thread
  (Opus decode, 256-frame blocks into `sceAudioOutOutput`, which paces itself).
- **Codec:** ask the host for HEVC: zero pictures of decoder delay at depth 1 and steadier decode
  times. Keep H.264 as the fallback and measure its delay with the real Remote Play stream.
  Use depth 1 for latency (depth 4 only buys throughput we don't need at 60 fps).
- **Display:** CPU conversion on 4 worker threads (about 3 ms per 1080p picture) is enough to ship
  phase 1. A GPU path through AGC (as ProsperoLight does) is the later step for 4K/HDR and to free
  the CPU.
- **Memory:** `kernel.flexibleMemorySize` 1 GiB in param.json; all large buffers from flexible or
  direct memory, not malloc; compute queue made once at start, before anything else.
- **Lifecycle:** hide the splash after the first flip, exit only through
  `sceSystemServiceLoadExec("exit")`, keep running in the background (stream can continue while
  the user is on the home screen), `sceSystemServicePowerTick()` every 10 s while streaming.
- **Category:** stay a game-category app: full speed, 12 GiB direct memory, can listen on a port,
  shows in Remote Play. Anything that must keep working while a game is in front belongs in the
  helper, not the app. Reconnect sockets whenever the app comes back to the front.
- **Install flow:** after registering a new title, wait a few seconds before the first launch, and
  report a failure instead of retrying in a loop (0x80940010 seen once right after registration).
- **Build:** this Docker build reproduces PS5Term's clean-room `libc.prx` byte for byte and needs
  nothing installed on the Mac but Docker; reuse it for `apps/play`.
