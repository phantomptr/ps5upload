# Gaps closed after the first builds (2026-11-27)

Short notes per gap: the options, the choice, and why. Measured results are added where they
were measured on the Pro (FW 13.60).

## 1. A host to join with one console (the second PS5 is away)

Options: (a) a native test guest in Rust against a real host (needs a second console as the
host); (b) the Pro hosting itself and the app joining it (the vpad user picker takes the Pro's
own controller, and the Pro would stream itself); (c) **a test engine that plays a recorded
Remote Play session as the live session of a console that is not there**, with a stand-in
helper.

Choice: (c). The recording goes through the same fan-out a live session uses
(`rp_stream_api::DevSession` wraps the stream's own `Sink`), so the guest socket, keyframe
pacing, timed video, the player's shallow queue and the host API behave exactly as with a
console. The stand-in helper (`rp_dev_replay.rs`) makes guests' pads, logs every button change
(the held chords show up as `pressed ps`), rumbles each guest's pad for a second every 10 s and
changes its light bar, so rumble can be checked from the app's log without anyone holding the
controller. A keyframe ask skips ahead to the next keyframe in the recording; a stop restarts
the session 5 s later (the guests' "host is starting again" path).

Developers only: `PS5UPLOAD_DEV_REPLAY_HOST=<file.psrp1>` on a test engine (own
`PS5UPLOAD_DATA_DIR` and port); `PS5UPLOAD_DEV_REPLAY_CONSOLE` (default `192.0.2.10`,
TEST-NET), `PS5UPLOAD_DEV_REPLAY_RUMBLE_S` (default 10, 0 off), `PS5UPLOAD_DEV_REPLAY_SEAT=stream`.
Nothing in any UI. Recordings: instant replay's `GET /api/ps5/rp/replay`, or
`scripts/make-psrp1.py` from any clip (an HEVC/H.264 elementary stream with AUDs, Opus sound).

## 2. HDR (HEVC Main10)

Facts: libSceVideodec2 decodes Main10 with the HEVC profile set to 2 (the app already asks for
2); the picture comes as two planes of 16-bit words with the 10 bits in the LOW bits (not
P010), so a row is twice the bytes (`frame_pitch_in_bytes == 2 * frame_pitch`), and the chroma
plane starts after the coded height (32-row blocks). sceVideoOut can show HDR10 directly
(format `0x8100070422000000`, R10G10B10A2 BT.2020 PQ, needs param.json `attribute` bit
`0x20000000`, which the app already has), but only a TV in HDR mode shows that right, and the
app would need the display's mode (`sceVideoOutGetOutputStatus`) and a second buffer set.

Options: (a) keep refusing and have the host switch the session to SDR for a PS5 guest (every
other guest of that session loses HDR too); (b) HDR passthrough to the TV; (c) **tone-map to
SDR on the CPU**.

Choice: (c) now, (b) later. Tone-mapping works on every TV and keeps one buffer format.
`core/hdr.cpp`: Y'CbCr (BT.2020 NCL, limited) to R'G'B' PQ codes in integers, PQ to linear by a
1024-entry table, BT.2020 to BT.709 primaries with an integer matrix (rows sum to 1, greys stay
grey), then one table that does the highlight roll-off (unchanged below 0.6 of the HDR
reference white, 203 cd/m2; an extended Reinhard curve above it that reaches 1.0 at an assumed
1000 cd/m2 peak) and the SDR gamma, indexed by a float-like log of the linear value. No maths
library is needed at run time (the tables use the app's own exp/log). The app now tells the host
it can show Main10 (`caps.main10`), so the host no longer gets a warning for it; the stats
report `codec` says `hevc.main10`.

## 3. The app finds its engine without the UDP probe

Options: (a) the `play_host` presence item (older PS5Upload lists every presence item as
"busy"); (b) a mesh document (shared with every device, outlives the engine being online,
written by several engines); (c) **a field in the engine's mesh hello** that older helpers
ignore and older apps never see.

Choice: (c). The engine says `"play": {port, join_ws, app_ws}` in its hello when it listens
beyond its own computer; the helper keeps it in memory only (never saved, never in
MESH_STATE) and `GET /v1/mesh` answers `engine` with that device and its port. A presence item
still works and wins. Older helper + new engine: ignored, the app probes as before.

## 4. Friends over Tailscale when the host uses PS5Upload's own Tailscale

See the engine's `tailnet_guest.rs` notes. Options: (a) serve the engine's port on the tailnet
straight to its loopback listener (every tailnet peer would look like this computer and get the
whole API); (b) a separate listener with only the guest routes, but peers still arrive from
127.0.0.1 (no per-address limits, no guard); (c) **the tailnet core writes a PROXY v1 line
with the real tailnet peer first, to a dedicated loopback listener that serves only
`/api/join/ws` and `/api/play/app`**, refuses anything without a valid line from Tailscale's
address range, and never maps a peer to loopback.

Choice: (c).
