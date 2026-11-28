# PS5Upload Play – phase 0 spike

A throwaway native PS5 app (title `FAKE99271`, "PS5Upload Play Spike") that checks the four hard
parts of the planned PS5Upload Play app on real hardware: controller input and rumble, hardware
H.264/HEVC decoding, putting pictures on screen, and sound. Measured results are in
[RESULTS.md](RESULTS.md).

## Build

Needs Docker (macOS or Linux):

```sh
apps/play-spike/build.sh
```

This builds the image in `docker/` (Ubuntu 24.04, clang/lld 18, ffmpeg), fetches the pinned public
payload SDK v0.42 into `.deps/` (hash-checked), rebuilds the clean-room `libc.prx` and checks it
against PS5Term's released digest, makes the two 5-second 1080p60 test streams with ffmpeg
(`testsrc2` with a frame counter, Annex B, an AUD before every picture) and their reference picture
hashes, and writes the app folder to `dist/FAKE99271/`. On a Linux host with the same tools,
`tools/build-in-container.sh` runs directly.

## Run

1. Copy `dist/FAKE99271/` to `/data/homebrew/FAKE99271` on the console.
2. Register it: `POST /api/ps5/app/register {"addr": "<ps5>", "src_path": "/data/homebrew/FAKE99271"}`.
3. Launch it: `POST /api/ps5/app/launch {"addr": "<ps5>", "title_id": "FAKE99271"}`.

At start the app runs its tests (decode speed and hashes for both codecs, conversion cost, a 2 s
tone, a short rumble), then plays the H.264 stream in a loop with the pad state drawn top right.
Everything is logged to `/data/ps5upload-play-spike/log.txt`; picture hashes go to
`avc-hashes.txt` and `hevc-hashes.txt` next to it (compare with `dist/*.ref`).

Commands: write one word to `/data/ps5upload-play-spike/cmd` (`exit`, `rumble`, `tone`,
`bgtone`, `avc`, `hevc`, `play-hevc`, `t1`, `t4`, `probe`).

For the background test the app also logs a heartbeat every second and runs a TCP echo on port
19271. A media-category build cannot listen, so it connects out to the address in
`/data/ps5upload-play-spike/peer` instead; `tools/echo-server.py` is the matching test server. The app creates that folder with its own owner: make it
writable for the helper first (`POST /api/ps5/fs/chmod {"path": "/data/ps5upload-play-spike", "mode": "777"}`).
`awake` calls a ShellCoreUtil function that ends the app; it is kept only to show that.

## Where the code comes from

- `tooling/native/`, `tooling/prospero-clang18`, `tools/make-stub.sh`, `tooling/libc.prx.sha256`
  and most of `tools/build-in-container.sh`: copied from PS5Term (GPL-3.0-or-later; the native
  app boilerplate is Copyright (C) 2026 BlackBearReloaded). Unchanged apart from the cut-down build
  script.
- `src/main.cpp`: our own code. The libSceVideodec2 structure layouts and call order follow the PS5
  VLC port's `ps5/modules/ps5vdec.c` (GPL-2.0-or-later, https://github.com/ZiZc3/VLC); the tiled
  framebuffer address function and the video-out/pad structures are PS5Term's.
