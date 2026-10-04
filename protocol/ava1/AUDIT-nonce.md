# AVA1 AEAD nonce / counter audit (review 006 #1, release gate)

Scope: Rust (`engine/crates/ava1`) and the console C (`payload/ava1`) at the p3-r006 branch.
Verdict: **PASS**. No (key, nonce) pair can be sealed twice. Three hardening items landed
with this audit: a counter ceiling on both stacks, a reworded comment, and tests that pin
the invariants.

## The invariant

An AVA1 AEAD nonce is `0x00000000 ‖ u64le(ctr)` (`keys.rs` `nonce()`, `ava1_noise.c`
`nonce12()`). Safety needs each `(key, ctr)` to seal exactly one frame. It holds because
(a) a key belongs to one connection and one direction, (b) each connection's counter starts
at 0 when it is keyed and only ever increments, and (c) a new key is derived for every
(re)join, so restarting at 0 never revisits a used pair.

## 1. Every `set_key` call site (Rust)

`grep -rn "\.set_key(" engine/crates/ava1/src` (non-test, before each file's test module):

| site | what | runs how often per live connection |
|---|---|---|
| `handshake.rs:190-191` (client) | control writer c2s, reader s2c | once, after the Noise handshake |
| `handshake.rs:299-300` (server) | control writer s2c, reader c2s | once, after the Noise handshake |
| `session.rs:519-520` | client lane writer c2s / reader s2c | once, on the JoinAck of that connection |
| `server.rs:839-840` | server lane reader c2s / writer s2c | once, after the JoinAck is sent |
| `conn.rs` `set_key` | the definition (zeroes `ctr`) | plumbing only |

Every other `set_key` is inside `#[cfg(test)]`. No data-plane or run-loop code calls it, so
there is no mid-stream re-key. `engine/crates/ava1/tests/lint.rs`
`set_key_is_called_only_where_a_connection_is_keyed` pins the exact production count per
file (handshake 4, server 2, session 2); a new call site fails the test until it is audited.

Each connection is keyed exactly once because each connection object is created, handed its
keys, and then moved into `drive()`; the reader and writer halves are not re-exposed.

## 2. Keys are direction- and lane-specific, and fresh per join

* Control: `control_key(c2s)` seals the client's writer; `control_key(s2c)` the server's.
  The Noise split gives distinct c2s/s2c keys and a new handshake gives new ones, so the
  control key is unique per handshake. `control_key = lane_key(dir, 0, 0^16, 0^16)`: the zero
  nonces are inputs to the *derivation* only (the old comment in `keys.rs` read as "the AEAD
  nonce is zero"; reworded).
* Lanes: `lane_key(dir, lane, cn, sn) = BLAKE2b-256(dir; "AVA1 lane" ‖ lane ‖ cn ‖ sn)`.
  `cn` is fresh random per join (`session.rs` `random_bytes`), `sn` is fresh random per join
  on the server (`server.rs` and C `ava1_server.c:994`), even for a replayed Join. Different
  direction, lane, or either nonce gives a different key (test:
  `a_resend_on_another_lane_never_reuses_a_key_nonce_pair`).
* Replayed Join: refused when the `cn` is in the session's recent window; older replays
  still get a fresh `sn`, hence a new key.
* Writer and reader are separate structs with their own `key`/`ctr`: a direction never
  shares a counter with the other, and the same counter value in c2s and s2c is under
  different keys.

## 3. The counter advances once per sealed frame, unconditionally

`FrameWriter::send_with_flags` (`conn.rs`) is the single sealing path (`seal_slice` has one
production caller). It seals with `ctr` and then increments, for every type: data, `Ping`,
`FLAG_IGNORABLE`. `FrameReader::recv` increments after every successful open. A failed write
after sealing leaves the counter spent, and `broken` is set before the write, so nothing is
sent after a half-written frame or a dropped future (a spent counter is never "unspent").
Test: `the_counter_stays_in_lockstep_across_frame_types_and_never_repeats`.

## 4. Frames resent on another lane after a lane death

The data plane keeps each in-flight frame's *plaintext* (`Frame.body: Arc<Vec<u8>>`) in
`Sched.inflight`. `lane_death` (`send.rs`) moves it to `requeue` with `resend = true`;
`lane_task` of any live lane picks it and hands the same plaintext to that lane's writer,
which seals it with that lane's own key and its own next counter. The dead lane's writer is
gone (its link tasks ended), its key and counter are never reused, and a re-joined lane
gets a brand-new key (section 2). Sealing never touches the shared body: `send_with_flags`
copies `header ‖ body` into one fresh `out` buffer and seals that copy in place, so the
`Arc<Vec<u8>>` (`FrameBody::Shared`) stays plaintext and a resend cannot see ciphertext or
reuse a keystream. Test `a_resend_on_another_lane_never_reuses_a_key_nonce_pair` seals the
same shared body on a dead lane, a re-joined lane and another lane and asserts: all nine
(key, nonce) pairs are distinct, the wire bytes differ, each stream opens only under its own
key, and the shared plaintext is unchanged.

## 5. Frame-buffer pool and zero-copy sealing (perf-lanes)

* `seal_slice` seals the sender's per-frame `out` buffer in place; that buffer is allocated
  per frame (`Vec::with_capacity`), not drawn from the receive-side pool. The pool
  (`ava1_frame.c` `ava1_frame_alloc/free`, `ava1_server.c:780`) holds *received* frames: they
  are opened in place (`ava1_conn_recv_body`) and then pooled for the apply path. Nothing is
  sealed out of a pooled buffer, and a pooled buffer is never re-used as an AEAD input with a
  stale nonce.
* The C writer seals in `send_frame_locked` under `wmu`: one lock, one counter, the seal and
  the increment in one statement. `ava1_conn_post` copies the body into a queue item, and the
  writer thread seals when it dequeues; ordering of counters follows wire order because both
  happen under `wmu`.

## 6. The C payload matches

* Nonce: `nonce12(n)` = 4 zero bytes then `u64le(n)`. Keys: `ava1_control_key`,
  `ava1_lane_key` (`ava1_keys.c`) are vector-checked against Rust by `ava1-ctest`.
* Counters (`ava1_conn.c`): `send_ctr++` inside `send_frame_locked` for every frame type, and
  `recv_ctr++` after every successful open. They are written nowhere else
  (`c_counters_are_only_incremented` in `lint.rs` fails on any other assignment). They are
  zero when the connection is `ava1_conn_init`ed, and the keys are installed once, in
  `ava1_server.c:880-882` (control) and `:1002-1004` (lane join), before the connection is
  used. A send that fails after the counter was spent marks the connection `broken`.
* **C nonce byte layout (review 007 #3, stated):** the 12-byte AEAD nonce is bytes 0-3 = `00 00 00 00` and bytes
  4-11 = the frame counter as a little-endian `uint64_t` (`nonce12()` in `ava1_noise.c`); the Rust side builds the
  same bytes in `keys.rs` `nonce()`. Both counters are 64-bit (`send_ctr`/`recv_ctr`, `u64`), so there is no width
  mismatch. It is pinned by `ava1-ctest` `aead.rs::the_c_nonce_is_four_zero_bytes_then_the_counter_little_endian` (the Rust
  side sealed with the nonce bytes spelled out, the C `ava1_seal` byte-identical at counters 0, 1, 0x0102030405060708,
  2^32 and the ceiling), and, through the connection, by `ava1_test_conn_open_frame` (a Rust-sealed frame opens in the
  C connection).
* The C `ava1_aead` counter wrap note (32-bit ChaCha block counter) is inside one frame
  (<= 16 MiB, far below 256 GiB) and is unrelated to the frame counter.

## 7. The second counter at `keys.rs:438`

It is in `#[cfg(test)]` (`the_published_noise_vector_reproduces`): the test replays the Noise
transport vector messages 3..5 with a local `n_i/n_r`. It is not production code and not an
AEAD nonce under a live key (production never uses Noise transport messages after the split;
the post-handshake keys are the derived `control_key`s, which differ from the raw split keys).

## 8. Hardening landed

* `NONCE_CEILING = u64::MAX - 1` (`conn.rs`) and `AVA1_NONCE_CEILING` (`ava1_conn.h`):
  a writer at the ceiling refuses to seal (`Ava1Error::Lost("nonce space exhausted")`; C breaks
  the connection and returns `AVA1_E_IO`), a reader at the ceiling refuses to open (`Lost` /
  `AVA1_E_TAG`). Increments use `checked_add`. It is unreachable in practice (2^64 frames);
  it turns a violated invariant into a clean connection break.
* `keys.rs` comment reworded (section 2). SPEC.md 4.4 now states the counter rules and the
  ceiling.

## Tests

* Rust: `conn::tests::{the_counter_stays_in_lockstep_across_frame_types_and_never_repeats,
  a_resend_on_another_lane_never_reuses_a_key_nonce_pair,
  a_writer_at_the_nonce_ceiling_refuses_to_seal, a_reader_at_the_nonce_ceiling_refuses_to_open}`;
  `tests/lint.rs::{set_key_is_called_only_where_a_connection_is_keyed,
  c_counters_are_only_incremented}`.
* C (host, via `ava1-ctest`): `the_c_connection_counts_every_frame_and_stops_at_the_nonce_ceiling`.
