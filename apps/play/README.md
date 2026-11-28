# PS5Upload Play

A native PS5 app (title `FAKE99261`) with two roles:

- **On a friend's PS5 (guest):** join a Play Together session by its 6-character code, or ask to
  join the game on a computer nearby, and play it full screen with your own controller: picture
  (hardware HEVC/H.264 decode), sound (Opus), rumble and light bar. Hold OPTIONS + TOUCHPAD for
  the host's PS button; hold L1 + R1 + OPTIONS + TOUCHPAD to leave.
- **On the host's PS5 (companion):** pair once with PS5Upload (a code typed into PS5Upload ›
  Play › Play Together › Pair PS5Upload Play), then see the session, let friends in or say no,
  share the invite code and QR code, see who is playing, and change Play Together's switches.

The app speaks PS5Upload's 21 languages: the PS5's system language when it is one of them
(else English), or the one picked in Settings (Hindi, Bengali and Persian, which the PS5 has no
system language for, are picked there). English is `src/ui/strings.hpp`; the others are
`i18n/<code>.json` (`python3 tools/i18n.py export` refreshes `i18n/en.json`, `check` runs in
`make test`). Latin, Cyrillic and Greek letters come from the UI font; Chinese, Japanese,
Korean, Arabic, Persian, Hindi, Bengali and Thai words are shaped at build time with the pinned
Noto fonts and shipped as small pictures (`assets/i18n/<code>.bin`), as PS5Term does.

The app only ever dials out (to the PS5Upload engine's `/api/join/ws` and `/api/play/app`, and a
UDP probe to port 19261 to find it); it never listens. It works without the PS5Upload helper;
when the helper runs, the app also uses its loopback app API (`127.0.0.1:9181`): the engines in
its mesh (Tailscale ones through the helper's forward), this console's Remote Play readiness,
turning Remote Play on (✕ twice) and a Remote Play pairing PIN.
Settings and the log are in `/data/ps5upload-play/`.

**Easy join** ([docs/EASY_JOIN.md](docs/EASY_JOIN.md)): the home screen lists games open on this
network ("Join Alex's game", one press) and hosts played with before ("Play again", one press, the
host still lets them in; kept in `friends.txt`), and shows a QR code for the phone: scan it, tap
"Play on my PS5" on the invite, and the app joins. For that the app hosts a small waiting room on
the signalling hub (`wss://hub.phantomptr.com/ws`, TLS by BearSSL, the room's secret in
`room.txt`) while the home screen is up, 5 minutes at a stretch. A host's PS5Upload can send an
invite there too ("Friends' PS5s"). `play-cli room URL SECRET_FILE` runs the waiting room on a
computer.

## Build

```sh
make -C apps/play            # dist/FAKE99261/ (Docker on macOS or Linux)
make -C apps/play release    # also dist/PS5Upload-Play-FAKE99261-<version>.zip (+ .sha256)
make -C apps/play test       # host unit tests and the network code against a stand-in engine
make -C apps/play shots      # every screen as a .bmp in build/host-tests/shots (after a build)
```

The version comes from `sce_sys/param.json` `contentVersion` (`13.000.000` is shown and named
`13.0.0`). The build fetches the pinned payload SDK, libopus and the ATRAC9 converter into
`.deps/` (each checked by hash or commit), rebuilds PS5Term's clean-room `libc.prx` and checks
its digest, makes the art, music and font, and links the app. `PLAY_NO_DOCKER=1 ./build.sh`
runs the same steps on a Linux host that has the tools in `docker/Dockerfile`.

`tests/play_cli.cpp` is the app's network side on a computer (`build/host-tests/play-cli join
ADDR PORT CODE`, `companion ADDR PORT --pair`, `discover`), handy against a real engine.

## Install by hand (development)

1. Copy `dist/FAKE99261/` to `/data/homebrew/FAKE99261` on the console.
2. Register it: `POST /api/ps5/app/register {"addr": "<ps5>", "src_path": "/data/homebrew/FAKE99261"}`.
3. Launch it: `POST /api/ps5/app/launch {"addr": "<ps5>", "title_id": "FAKE99261"}`.

With a file `/data/ps5upload-play/dev` present, the app also reads one command at a time from
`/data/ps5upload-play/cmd`: `join ADDR PORT CODE`, `nearby ADDR PORT`, `host ADDR PORT`, `pick`, `pickhost`, `pair`,
`drop` (let the socket go, to test resuming), `leave`, `home`, `exit`.

Measured facts from the phase 0 spike are in [docs/PHASE0.md](docs/PHASE0.md), microphone capture
in [docs/MIC.md](docs/MIC.md); where the code
comes from is in [NOTICE.md](NOTICE.md).
