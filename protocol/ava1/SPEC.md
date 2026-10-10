# AVA1 wire protocol, version 1 (normative)

AVA1 = Adaptive Verified Assembly. Design rationale: ps5upload-docs
`superpowers/specs/2026-09-30-ava1-transfer-protocol-design.md`. Implementations:
`engine/crates/ava1` (Rust), `payload/ava1` (C). Both must pass `protocol/ava1/vectors/`.

## 1. Transport
TCP, port 9120. A session is one control connection (lane 0) plus up to 8 data
connections (lanes 1..=8). Integers are little-endian.

## 2. Frame header
16 bytes: `"A1"` (0x41 0x31), type u8, flags u8, channel u32, body_len u32,
CRC32C (Castagnoli, reflected, init/xorout 0xFFFFFFFF) of bytes 0..11 as u32.
body_len ≤ 16 MiB (receivers may enforce a lower cap: 64 KiB on control
connections). Bad magic or CRC: close the connection. Flags: bit 0 SEALED (the
body is AEAD ciphertext + 16-byte MAC, §4.4), bit 1 IGNORABLE (a receiver that
does not know the type skips the frame).

## 3. Message encoding
Messages are defined in `schema/ava1.toml` and generated for each language by
`ava1-gen`; never hand-encode. A body is the message's fixed fields in schema
order — u8/u16/u32/u64 little-endian; b16/b32 raw; bytes = u32 length + bytes;
str = u16 length + UTF-8 — followed by an extension block: u16 count, then per
extension u16 tag, u32 value length, value (encoded as a field of its type).
Encoders write extensions in ascending tag order. Decoders skip unknown tags,
reject a repeated known tag, reject invalid UTF-8, and reject trailing bytes
(both in the body and inside an extension value). The canonical encodings in
`vectors/messages.txt` must round-trip byte for byte.

Decoders accept a superset of what encoders write — inside a records item, an
extension may be unknown or out of order, and the decoder drops it — so a
re-encoder is only bound to canonical input, which is all an encoder ever
produces: it must reproduce canonical bytes exactly. For a message it accepted but
would not itself have written, a re-encoder may reproduce the captured item bytes
verbatim (the C decoder holds them) or re-encode each item canonically (the Rust
one does); the two agree wherever the input is canonical, and nothing in the
protocol re-encodes a peer's bytes.

**Records.** A field of type `records` holds a list of one struct — an *item* is that
struct's encoding per this section (its fields, then its extension block), and the
length written before it is exactly that. It is encoded as a
`bytes` field whose content is, for each item in order, `u32le(item length) ‖ item`.
An empty list is `00000000`. A decoder validates every item (a bad item makes the whole
message malformed). Records are not allowed as extensions. C decoders keep a pointer to
the encoded list and its item count; items are read with the generated `ava1_<struct>_next`.

## 4. Keys and sealing
4.1 Identity: a static X25519 key pair per node.

4.2 Handshake: `Noise_XX_25519_ChaChaPoly_BLAKE2b` (Noise revision 34), prologue
"AVA1 v1"; the client is the initiator. Implementations must reproduce
`vectors/noise_xx.json` (from the cacophony set). After message 3, Split() gives
c2s (initiator → responder) and s2c; `h` is the handshake hash. Low-order keys:
an implementation must abort the handshake when a DH result is all zero —
equivalently, when the peer's ephemeral or static key is a low-order point (the C
side checks each DH output; the Rust side checks each received key, since its Noise
library does not). A handshake step that fails poisons the state: no later message
is read or written and no keys are derived from it. Key material is wiped after use.

4.3 Lane keys: lane_key(dir, n, cn, sn) = BLAKE2b-256(key = dir, "AVA1 lane" ‖
u16le(n) ‖ cn ‖ sn), where cn and sn are the 16-byte client and server nonces of
the lane's join (§9). Both are fresh random per join, so a re-join of lane n — or a
replayed Join, even one the server no longer remembers — never gets a key already
used with counters restarted at 0. The control connection is lane 0, keyed once per
handshake, with cn = sn = 16 zero bytes. `vectors/keys.txt` pins these derivations.

4.4 Sealed frames: body = ChaCha20-Poly1305(lane key of this direction, nonce =
4 zero bytes ‖ u64le(counter), AD = header bytes 0..11) followed by the 16-byte
MAC; the counter is per lane and direction from 0, advances by one on every sealed
frame of any type (Ping and ignorable frames included), and is never reset or rewound
while a key is in use. A frame resent on another lane after a lane death is sealed again
under that lane's own key and counter. A counter that reaches 2^64 - 2 may not seal or
open another frame: the sender refuses and the receiver closes the connection, so a
nonce can never repeat under one key. A frame that fails to open closes the connection.

4.5 Join proofs: BLAKE2b-128(key = BLAKE2b-256(key = dir, "AVA1 join"), m):
the Join tag uses dir = c2s and m = "join" ‖ session_id ‖ u16le(lane) ‖ cn; the
JoinAck tag uses dir = s2c and m = "join-ack" ‖ session_id ‖ u16le(lane) ‖ cn ‖ sn.

4.6 Pairing PAKE. Pairing proves, to the console, that the person at the app has read the
console's screen. The console draws a random six-digit code from its CSPRNG for each
unconfirmed session and shows it in its notification ("enter 123456 in the app"); the code is
never derived from the transcript and never sent. It is the password of a CPace-style PAKE run
inside the encrypted session (messages `PairPakeClient`, `PairPakeServer`, `PairConfirm`,
`PairResult`, §5.5). With h the Noise handshake hash (64 bytes) and `code` as six ASCII digits:

* `d = BLAKE2b-256("AVA1 CPace" ‖ h ‖ code)`; `G = Elligator2(d)`, the u coordinate of
  Curve25519, by the map of Monocypher's `crypto_elligator_map` (RFC 9380
  `map_to_curve_elligator2_curve25519`, Z = 2; the top two bits of `d` are ignored).
* each side draws a fresh 32-byte scalar x and sends `Y = X25519(x, G)` (X25519 clamps x); a
  result of all zero bytes is refused.
* `K = BLAKE2b-256("AVA1 CPace K" ‖ h ‖ X25519(x, Y_peer) ‖ Y_client ‖ Y_server)`; an all-zero
  shared secret (a low-order `Y_peer`) is refused.
* key confirmation: `MAC_client = BLAKE2b-256(key = K, "client" ‖ h)`, sent in `PairConfirm`;
  `MAC_server = BLAKE2b-256(key = K, "server" ‖ h)`, returned in `PairResult` only when the
  client's verified. Every comparison is constant time.

`vectors/cpace.txt` pins G, both public values, K and both MACs; the Rust and C implementations
both reproduce it, and a differential test compares the two Elligator2 implementations on
thousands of inputs.

Threat model. The code is a secret that exists only on the console's screen. A host on the LAN
can complete Noise with a throwaway key and be welcomed (it sees h, both nonces, every frame),
but none of that depends on the code, so it cannot compute `G`, hence not `K`, hence not a
valid `MAC_client`: each attempt is one online guess at one in 10^6, and there is nothing on the
wire to test a guess against offline (`Y = x·G` hides G behind a discrete logarithm). A man in
the middle holds two handshakes with different h, and the code is shown by the console, not by
the app; what the app sends is bound to the first leg's h and the console's code, so it
verifies on neither leg without the code.

Remaining limits, stated plainly:

* A fake console (an impostor at the console's address) gets one guess per client attempt: the
  app's `MAC_client` is checked against the impostor's own key, which the impostor can test
  offline for a guess at the code. A correct guess (probability 10^-6 per attempt) reveals the
  code, and the impostor can then answer with a valid `MAC_server`. The real console never
  sees these attempts, so it cannot count them; the client is the only place they could be
  limited, and a person retyping codes is not a fast guesser.
* Someone who can see the console's screen, or the user's typing, knows the code. Out of scope.
* A user who types the code of a pairing they did not start has paired the other party.
* Denial of service on a LAN, and what bounds it. Only a real guess (the PAKE was exchanged and
  the proof was wrong) counts as a failure: a throwaway session that never completes the
  PAKE, sends a `PairConfirm` with nothing behind it, or opens with a malformed or low-order
  value reveals nothing and costs no budget. Each source address has 5 wrong guesses per
  window, and all addresses together 20 (about 2·10^-5 of the space); the 20th closes the
  window until a paired device reopens it or the node restarts, so closing it takes guesses
  from at least four addresses. Sessions that guess nothing are bounded by `MAX_UNPAIRED`
  (2) and by 6 new pairing sessions per address per 10 s (`ERR_BUSY` beyond that). The
  console shows every welcomed session's own code on its screen (an identical request, same
  address and same key within 10 s, is not shown twice; the global notice rate below): a stranger cannot hide the user's
  code, only crowd the two unconfirmed places for up to 60 s each. The guess is taken from
  the budgets under one lock before the proof is looked at, so two sessions confirming at once
  cannot spend the last guess twice, and a session whose address has no guess left is refused
  without its proof being compared. A budget is never evicted from a full table: a node
  tracking many addresses refuses new pairing sessions (`ERR_BUSY`) from unseen addresses
  while every slot holds guesses made in this window (C: 32 slots), instead of handing a
  cycling attacker a fresh budget; IPv4-mapped IPv6 addresses count as their IPv4 form.
* Residuals of the per-address budget. An IPv6 host with privacy (temporary) addresses, or a
  host with several aliased addresses, appears as several addresses and so has several
  per-address budgets; the global cap of 20 guesses per window still bounds everything
  together, and the 20th closes the window. Spoofed or many hosts can likewise spend the global
  cap (a denial of service, recovered by a paired device's `pairing.open` or a restart), but
  cannot raise the number of guesses beyond it.
* Notification bounds. Besides the per-session code and the identical-repeat rule, all
  addresses together may show a burst of 3 pairing notifications and then one per second.
  Past that, the newest session's code waits for credit (at most about a second) and any
  older session still waiting is dropped, so the code on the screen is always the latest
  attempt's, and the screen cannot be flooded faster than one notification per second.

The `pair_commit`, `nonce_c` and `nonce_s` fields of §5 are retained for wire stability; no code
is derived from them any more.

## 5. Handshake and pairing
1. Client → `Hs1{noise}` (unsealed): Noise message 1, payload `HelloInfo`
   (version range, caps 0).
2. Server: no common version → `Error(ERR_UNSUPPORTED_VERSION)` unsealed, close.
   Else → `Hs2{noise}`: message 2, payload `ServerInfo` (version, caps, random
   session_id, pair_commit (§4.6), name). `caps` bit 0 is `CAP_DATA_PLANE` (1): the node hosts the jobs
   of §11–§16. A client sends no data-plane frame and no method 16–19 request to a
   node that did not advertise it.
   Bit 1 is `CAP_MGMT` (2): the node serves the management methods of §7.3 (numbers 4 and
   up other than 16–19). A client routes management calls by this bit instead of probing
   for `ERR_UNKNOWN_METHOD`; a node that does not advertise it answers
   `ERR_UNKNOWN_METHOD` to them. The payload advertises it when its management table is
   installed (`mgmt_rpc_installed()`); the Rust `Session::has_mgmt()` reads it.
3. Client → `Hs3{noise}`: message 3, payload `ClientInfo` (nonce_c (§4.6), name). Both sides
   now key lane 0 (§4.3) and every further frame is sealed. A client that expects
   a particular device (it knows the key it paired with at this address) compares
   the server's static key from message 2 and, if it differs, closes without
   sending `Hs3` — the wrong device never learns the client's key or name.
4. The server learned the client's key in message 3. Unknown key and pairing
   closed → sealed `Error(ERR_PAIRING_CLOSED)`, close. A ClientInfo without
   nonce_c → sealed `Error(ERR_PROTOCOL)`, close. Else → sealed
   `Welcome{knows_you, nonce_s}`. The client checks BLAKE2b-256(nonce_s) against the
   pair_commit of message 2 before it shows any code or trusts anything else in the
   Welcome; a mismatch, or a ServerInfo or Welcome without its field, closes the
   connection (after a best-effort `Error(ERR_PROTOCOL)`).
5. Pairing (§5.5, PAKE over the Noise session; §4.6): while the console does not know the
   client it shows the session's random code on its screen, and the user types it into the
   app. A client whose server sent knows_you = 0 sends `PairPakeClient{y}` (channel = request
   id) and the server answers `PairPakeServer{y}` on that channel (once per session; a second
   `PairPakeClient`, or one while the window is closed, ends the session). The client then
   sends `PairConfirm{mac}` and the server answers `PairResult{accepted, mac}`. The server
   accepts only while its pairing window is open and the client's `mac` verifies (and its
   owner, where there is a hook, does not veto), then stores the client's key (a key that
   cannot be stored is not accepted) and returns its own `mac`; the client stores the
   server's key only if `accepted` is set and the server's `mac` verifies. Otherwise
   `accepted = 0` with a zero `mac`, and the session ends: one attempt per session. A
   `PairConfirm` before the PAKE, one that does not decode (the old empty body), or a
   malformed or low-order `PairPakeClient` is refused and ends the session, but guessed nothing
   and so is not counted. A wrong `mac` after the PAKE ran is a guess: it is logged per source
   address and counted against that address's budget (5 per window) and the global one (20
   per window); the 20th closes the window until a paired device reopens it or the node
   restarts, and an address that spent its budget is refused at `PairPakeClient` until then.
   A failure makes that address's next welcome show a new notification at once. New pairing
   sessions are also rated: at most 6 per address per 10 s (`ERR_BUSY` beyond that), and
   `MAX_UNPAIRED` (2) at a time. Every welcomed session shows its own code on the
   console; only an identical repeat (same address and key) within 10 s is not shown again.
   The client cannot tell a wrong code from a typo, only that the console
   refused; its next try is a new handshake with a new code on the screen. A client that cannot
   be told apart from a paired one (a trusted reconnect, §5.1, or a launch proof, §5.2) never
   sends these messages and needs no code. Until accepted, RPCs answer
   `ERR_NOT_PAIRED` and lanes are refused. A client sends nothing but the pairing messages (no RPC, no Join) while
   either side is unconfirmed. A data-plane frame (§11–§16) on a
   control connection whose pairing is not accepted is a protocol error: the server answers a sealed
   `Error(ERR_NOT_PAIRED)` and closes the connection.
6. Pairing window: opens by itself for 5 minutes after start only while the node
   has no paired peer; otherwise `pairing.open` (method 2, body `PairingOpen`,
   ≤ 600 s) from a paired session opens it. Either kind of window closes as soon
   as one pairing succeeds. A session welcomed with knows_you = 0 that has not
   been accepted ends — sealed `Error(ERR_PAIRING_CLOSED)`, close — when the
   window closes or 60 s after its Welcome, whichever is first. At most 2 such
   sessions exist at a time; a third unknown client gets `Error(ERR_BUSY)` in
   place of Welcome. A node shows each welcomed session's own code (an identical
   request, same address and key, not twice within 10 s), and only for a client it has sent
   Welcome to: a client gone before its Welcome uses none.
7. Peer stores: `<64 hex key> <unix seconds> <name>` per line, ≤ 32 peers (oldest
   dropped), written atomically (a temp file no other writer shares + rename in the same
   directory). The reference side shares the file, the identity and the launch tokens between processes:
   each read-modify-write holds an advisory lock on `<file>.lock` and starts from the file as it is then. A
   missing file is an empty store. A file that exists but cannot be read is not:
   the node runs, knows no peers, logs the failure, never opens its automatic
   window, accepts no pairing, and never writes the file.

5.1 Trust slot: the payload ELF carries a 64-byte array — "AVA1TRUST" (9 bytes),
state (0 empty, 1 key, 2 key + launch token), 6 zero bytes, 32-byte X25519 key,
and a 16-byte launch token in state 2 (§5.2), zero otherwise. An engine sending
the ELF writes its key into the single slot; the payload adds that key to its
peers at startup. Exactly one slot must exist. A slot counts as a slot in states
0 and 1 only while its last 16 bytes are zero, so data that happens to start
"AVA1TRUST" is not one; state 2 is identified by its state byte.

5.2 Launch token: whoever stamps the ELF may also write a fresh random 16-byte
token into it (state 2) and keep it for itself — the reference side stores them in
`<data dir>/ava/launch_tokens`, one `<32 hex token> <unix seconds>` per line, mode
0600, written atomically, at most 32 kept (oldest dropped), each good for 10 minutes and for one proof.
The payload keeps the token in memory only. When the handshake's client static key
equals the slot's key, and the client is one the server knows — a helper whose
peers file could not be written knows nobody and sends no proof — the server adds an
ignorable extension field `launch_proof` to `Welcome`: the first 16 bytes of
BLAKE2b-256(key = the token followed by 16 zero bytes, "AVA1 launch" ‖ h), h
being the handshake hash. No other client gets a
proof, a proof differs every handshake, and the token itself never travels.
`vectors/launch.txt` pins the derivation.

A client that recognises the proof — one of its unexpired tokens, on this
handshake — spends that token (removes it and saves the removal before it trusts
anything; if the removal cannot be saved, the proof is not accepted) and stores the server's key and treats the session as paired with no
pairing code. A proof counts only from a server it does not already know and
whose `Welcome` says it knows the client (`knows_you` ≠ 0); a server that does not
know the client was never given a proof, so one presenting a proof anyway is not
trusted. A client that does not recognise it (another token, expired, an old proof
replayed under a new h, a token already spent, or a `knows_you` of 0) pairs with the code as usual
(§5.5: the user types the code the console shows). The token proves "this console is the helper I
launched" to the side that sent it. The ELF is sent unauthenticated, so a sniffer on the path holds
the token too and could answer as the console to the launching engine: that is why a token is
single-use and short-lived. The first proof wins, and the real helper normally connects within
seconds of the launch, so a replay after that finds the token gone; an attacker who answers first
(before the real helper, within 10 minutes) gets the one silent pairing and nothing more. A proof is
needed only from a server not yet known, so nothing uses a token twice.

## 6. Liveness
Every connection sends `Ping{seq, t_us}` every 2 s (default) on channel 0, also
while it is in the middle of reading a large frame; the receiver answers `Pong`
with the same values; the sender's RTT is now − t_us. t_us is taken when the Ping is
written, not when it is queued, so time spent behind other frames is not counted as
round trip. A sender may skip a Ping or Pong while other frames are queued or being
written: they are proof of life too.

Liveness counts bytes, not frames: a connection is dead after 12 s (default,
`dead_after`; the ping stays at 2 s, so six pings go unanswered first) with no byte received, so a 16 MiB frame on a slow link is never
mistaken for silence. Silence is judged by what can be read: a reader that was busy
elsewhere (writing, waiting to write) past `dead_after` checks the socket first and
carries on if bytes are waiting; a process that was not running (a late timer tick)
may put off the verdict for at most 2 ticks in a row. Each frame must also move at no less than a rate floor
(default 8 KiB/s) after a `dead_after` grace — its deadline is dead_after +
body_len / floor — so a peer cannot drip one frame forever. Writes obey the same
two limits: a peer that takes no bytes for `dead_after` (it stopped reading), or
takes one frame slower than the floor, has its connection closed. A reader never
waits on a socket write: replies are queued to the connection's writer (bounded),
and a peer whose replies back up is disconnected.

Clocks are monotonic. A server closes a connection whose handshake (first byte
to Welcome) does not finish within the handshake timeout (10 s default). `Bye`
ends a session; `Error` reports why and ends the connection.

## 7. RPC
`RpcRequest{method, body}` on the control connection, channel = request id
(chosen by the client, unique among its outstanding requests). The server
answers `RpcResponse{status, body}` on the same channel; a request that does not
decode is answered `Error(ERR_PROTOCOL)` and closes the connection. status 0 (`STATUS_OK`) = OK;
error statuses are the `ERR_*` constants, and an error response's body is the cause as
UTF-8 text (not an encoded message). An unpaired session's RPCs answer `ERR_NOT_PAIRED`; a
session has at most 8 requests in flight (§7.4) and the next one answers `ERR_BUSY`. Only
`pairing.open` is answered on the reader; every other method runs on a worker, so a slow method never
delays liveness.

7.1 Methods:

| # | name | request body | response body (status 0) |
|---|------|--------------|--------------------------|
| 1 | `node.info` | empty | `NodeInfo{version, platform, name}`, ext `firmware` |
| 2 | `pairing.open` | `PairingOpen{seconds}` (≤ 600) | empty (§5.6) |
| 3 | `crypto.bench` | `CryptoBench{mib}` | `CryptoBenchResult{bytes, micros}`, ext `open_micros`, `backend` (a diagnostic) |
| 16 | `job.copy` | `JobCopy{job_id, src, dest, flags}` | `Status` (§16.9), ext `state` |
| 17 | `job.status` | `JobRef{job_id}` | `Status`, ext `state` |
| 18 | `job.cancel` | `JobRef{job_id}` | empty |
| 19 | `disk.calibrate` | `DiskCalibrate{dir, files, size}` | `DiskCalibrateResult` (§16.10) |
| 22 | `c2c.allow` | `C2cAllow{job_id, key, root}` | `C2cTicket{token}` (§18) |
| 23 | `c2c.send` | `C2cSend{job_id, host, port, key, token, src, dest, flags}` | `Status`, ext `state` (§18) |
| 4–141 | management methods | see §7.3 and `MGMT_METHODS.md` | see §7.3 |

`Status.state` is 0 while the job runs, 1 when it finished OK and 2 when it failed (the cause is
in ext `current`, the `ERR_*` code in ext `code`; a finished `job.run` job's output is in ext `result`). Methods 16–19 are the version 1 data-plane RPCs and exist only on a node that
advertises `CAP_DATA_PLANE`; the management methods are §7.3 and exist on a node that advertises `CAP_MGMT`. The behaviour of 16–18 is §15.5; of
19, §16.10.

7.1.1 `job.run` and `job.list` (long management operations). `job.run{job_id, op, args}` starts operation
`op` (`JOB_OP_*`: DELETE 1, CHMOD_R 2, HASH 3, CRC32 4, FSCK 5, BACKUP_SNAPSHOT 6, BACKUP_RESTORE 7,
CLEANUP 8, SDK_SCAN 9) on its own worker thread (the 512 KiB management stack) and answers at once with a
`Status` (state 0). `args` is the operation's request body, the same legacy JSON the FTX2 frame carried
(at most 60 KiB). The job is an entry of the job table (counted against the 32-job limit, owned by the
peer that started it, no session, so a reconnect does not matter), at most 8 operations run at once
(`ERR_BUSY` for a ninth) and an unknown `op` is `ERR_PROTOCOL` (`unknown_op`).
* `job.status` returns the progress (`files_done/total`, `bytes_durable/total`: files are non-directories,
  bytes the regular files' sizes; both totals are 0 while unknown), the current step in ext `current`, and,
  once finished, `state` 1 with ext `result` (the operation's reply body, at most 128 KiB) or `state` 2 with
  ext `code` (the `ERR_*`) and the cause token in `current`. A repeat of `job.run` with the same id and the
  same owner, op and args answers the job's status whatever state it is in; other parameters are
  `ERR_PROTOCOL`, another owner `ERR_UNKNOWN_JOB`. A finished operation whose repeat gives the same outcome
  (every op but BACKUP_SNAPSHOT and BACKUP_RESTORE) stays listed for a grace of 10 s after the first reply
  that carried its terminal status, so a reply lost on the wire is answered from the stored job and nothing
  runs twice inside the grace; it is then released by the reaper or by the next `job.status`/`job.run` read
  after the grace, and a full job table releases the one delivered longest ago at once, so a loop of hashes
  never fills the 32 slots. After the release a repeat of `job.run` runs the operation again (harmless for
  delete, chmod, hash and crc32). A backup is kept for the park age instead, because a re-run would take a
  second snapshot.
* `job.cancel` raises the job's cancel flag and returns at once (it runs on a reader or RPC worker and
  never waits for the worker). A delete, chmod, hash, crc32 or backup stops at the next directory entry or
  read block and the poller then sees `state 2` with `ERR_CANCELLED`; fsck, cleanup and sdk.scan are one
  system call and only honour a cancel that arrives before they start, so a cancelled fsck holds its job
  slot (and its worker) until the system call returns. Unlike a copy, a cancelled operation stays listed
  (finished) so a poller reads how it ended; it is collected like any finished job.
* An operation that wraps an FTX2 handler (fsck, backup, cleanup, sdk.scan) keeps the handler's
  `{"ok":false,...}` body as its result: the operation ran and the body is the answer. Only an ERROR
  frame is a failed job. DELETE refuses a path outside the writable roots and a mount point (a path on
  another device than its parent): `ERR_PATH`, `fs_delete_path_not_allowed` / `fs_delete_path_is_mount_point`.
* `job.list` returns the peer's jobs of every kind as `JobListResult` (`JobEntry.kind` 1 upload, 2
  download, 3 copy, 4 operation).

7.2 Error codes. The numbers below are generated from `schema/ava1.toml`, whose constants are the
normative table; the second column names the constant in the generated code.

| code | name | sent when |
|------|------|-----------|
| 1 | `ERR_NOT_PAIRED` | an RPC or a lane join from a session whose pairing is not accepted (§5) |
| 2 | `ERR_PAIRING_CLOSED` | an unknown client while the pairing window is closed; an unconfirmed session whose window or 60 s ended |
| 3 | `ERR_UNSUPPORTED_VERSION` | the version ranges of the two peers do not overlap (unsealed, §5) |
| 4 | `ERR_PROTOCOL` | a frame or body that does not decode, a frame type not allowed where it arrived, a `JobOpen` with an unknown kind, policy or flags, a `Chunk` for a small file or a `BundleRecord` for a large one (§12.2), a window that cannot hold one group (§12.4) |
| 5 | `ERR_BAD_JOIN` | an unknown session, lane id outside 1..=8, wrong tag or replayed nonce (§9) |
| 6 | `ERR_UNKNOWN_METHOD` | `RpcResponse` status for a method the node does not implement, including methods 16–19 on a node without the data plane |
| 7 | `ERR_INTERNAL` | the node could not do what the peer asked for a reason that is neither the peer's nor the disk's: out of memory, a thread that would not start |
| 8 | `ERR_BUSY` | a limit of §8 or §11.7: connections, sessions, an unconfirmed-session slot, in-flight RPCs (8 per session, §7.4), jobs, a destination another job is writing, no buffer budget left for another job |
| 9 | `ERR_PATH` | a manifest or RPC path that breaks §11.2, a root the node's write or read policy refuses, a source that cannot be stat'd or a staging parent that is not a directory |
| 10 | `ERR_NO_SPACE` | the destination drive is full (`ENOSPC` while writing) |
| 11 | `ERR_UNKNOWN_JOB` | `Resume`, `job.status` or `job.cancel` for a job the node does not list, or lists for another peer key (the two are not told apart) |
| 12 | `ERR_IO` | a disk or filesystem failure on the node's side: write, fsync, rename, journal append, reading a source, a failed `disk.calibrate` |
| 13 | `ERR_VERIFY` | a file or copy whose bytes do not match their root and that a `FileRetry` cannot fix |
| 14 | `ERR_EXISTS` | a destination that is already there and may not be replaced: the root without `JF_OVERWRITE`, a staging root that appeared meanwhile, a file where one must go in a merge |
| 15 | `ERR_CANCELLED` | the job was cancelled (`job.cancel`, or a `JobCancel` carrying this reason) |
| 16 | `ERR_CROSS_DEVICE` | a staged or part-file rename whose two sides are on different devices (`st_dev`); never attempted, because a cross-device `rename` panics the console's kernel |
| 17 | `ERR_CREDIT` | a lane frame larger than the credit the receiver granted (§12.4) |
| 18 | `ERR_STALLED` | the receiver ended a job whose sender sent no file data for the progress deadline while heartbeating (§12.8) |
| 19 | `ERR_PAIRING_CODE` | a client-side refusal of the pairing: the console refused the typed code, or could not prove it knows it (§5.5) |

7.3 Management methods (the console operations FTX2 carried on :9114). Numbers are assigned by
block; the tracked list, one row per FTX2 frame with its payload handler and engine caller, is
`MGMT_METHODS.md`. The generated constants `METHOD_*` in `schema/ava1.toml` are normative.

| numbers | block | bodies |
|---------|-------|--------|
| 4–11 | node and diagnostics: `node.status`, `node.shutdown`, `node.cleanup`, `log.klog`, `log.syslog`, `net.interfaces`, `net.reach`, `net.speedtest` | `node.status` replies `NodeStatus`; the rest `MgmtText` |
| 20–21 | `job.run`, `job.list` | `JobRun{job_id, op, args}` → `Status` (ext `state`, `result`, `code`); `job.list` → `JobListResult` |
| 32–44 | filesystem: `fs.volumes`, `fs.list`, `fs.stat`, `fs.mkdir`, `fs.rename`, `fs.chmod`, `fs.read`, `fs.write`, `fs.mount`, `fs.unmount`, `fs.mount_pkg`, `fs.mount_lwfs`, `fs.freespace` (44) | `fs.list`, `fs.stat`, `fs.freespace` (`FsPath` → `FsFreeSpace`: usable bytes = free less the working margin, 1/64th of the drive at most 1 GiB; the nearest existing ancestor of the path is asked), `fs.mkdir`, `fs.rename`, `fs.chmod`, `fs.read`, `fs.write` are typed (`FsList` → `FsListResult`, `FsPath` → `FsStat`, `FsMkdir`, `FsRename`, `FsChmod`, `FsRead` → `FsReadResult`, `FsWrite`); the others `MgmtText` |
| 48–61 | apps, launch, install queries, processes | `MgmtText`; `app.list` pages with `offset`/`limit` and `more` (§7.4, the only text method that does not fit one reply) |
| 64–70 | saves, screenshots, videos, search index | `MgmtText` |
| 72–87 | hardware, power, time, peripherals, `shell.exec` | `MgmtText` |
| 88–100 | profiles, users, backups (97 and 99 are unassigned: backup snapshot and restore run as `job.run` ops) | `MgmtText` |
| 104–128 | cheats, SMP metadata, SDK changer, TMDB, FTP, firmware spoof, notifications, activity | `MgmtText` |
| 136–141 | Remote Play | `MgmtText` |

A `MgmtText` body is the payload handler's existing request or reply (UTF-8 text, in practice
JSON), carried unchanged in `MgmtText.body`; `more = 1` on a reply means the method is paged and
the caller asks again with the next `offset`. Typing the text methods is deferred (§10): the text
bodies are stable and tested, and the cutover does not need them typed.

`log.klog` and `log.syslog` are clamped tails, not paged reads: when the console's text is longer than
`RPC_TEXT_MAX` the reply is its newest `RPC_TEXT_MAX` bytes, starting at a line boundary (else a
UTF-8 boundary), with `more = 1` meaning "older text was left out"; a text that fits is returned whole
with `more` absent or 0. `net.reach` is a probe: its negative answer (`{"ok":false,"timed_out":..,
"errno":..,"ms":..}`) is the measurement and travels as an ordinary OK reply; only a malformed request
(`bad_request`, `bad_address`) is `ERR_PROTOCOL`. A console writes a human-readable job event log at
`/data/ps5upload/ava/events.log` (one line per job open, resume, done and fail with status, bytes,
files and lanes; 1 MiB, rolled to `events.log.old`), read with `fs.read` like the other log files.

Encoding overhead. A `MgmtText` is `u32 length + text + u16 ext count` (6 bytes), plus 7 bytes when
`more` is present (tag u16, length u32, value u8). It is the `RpcResponse` body, so the largest text
a handler may return is `RPC_REPLY_MAX - 16 = 262,128` bytes (`RPC_TEXT_MAX`, with 3 bytes to spare);
a text request is bounded the same way by 56 KiB. Typed bodies carry their own overhead
(`FsReadResult` is `data + 7`).

Errors: the response status is an `ERR_*` code (§7.2) and the body is the cause as UTF-8. A ported
handler's cause is its legacy token (`fs_move_cross_mount`, `cleanup_path_denied`, ...), so the
engine can build the same `payload rejected <LABEL>: <cause>` text FTX2 callers produced. No new
error codes were added for management methods. `fs.rename` answers `ERR_CROSS_DEVICE` when the
source and the destination's parent are on different devices (`st_dev`); it never calls `rename(2)`
across devices. A device that cannot be read (unknown) is refused too, with `ERR_IO` (`fs_move_device_unknown`):
only a definite "same" reaches `rename(2)`. Every `st_dev` guard (§11.6, §12.6, `fs.rename`, FTP, shell `mv`) fails closed.

Legacy failure bodies. Many FTX2 handlers answered a failure as a *successful* frame with a
`{"ok":false,"err":"..."}` body (`handle_fs_write_bytes`, `handle_net_reach`, `handle_toast_send`,
the TMDB, SDK and cheats handlers, ...). A ported handler never does that: it answers an `ERR_*`
status (the closest of §7.2; `ERR_INTERNAL` when none fits) with the legacy token as the cause, and
`STATUS_OK` only when the operation succeeded. A body that still contains `"ok":false` under
`STATUS_OK` is a porting bug. Where the legacy body also carried data on failure (a partial list,
a detail object), the cause is that body's `err` token and the data is dropped.

Truncation. A ported handler must detect truncation and fail loudly. Every `snprintf` into a
reply buffer is checked (`n < 0` or `n >= cap` is an error, never clamped to `cap - 1` and sent), a
clamped read (`klog`, `syslog`, `fs.read`) reports a short read as a short read (`eof`, `more`), and
a buffer that cannot hold the whole answer answers `ERR_INTERNAL` with the cause `reply truncated`.
The payload helper is `ava1_rpc_text(out, cap, &out_len, fmt, ...)` (`ava1_data.h`): it returns
`STATUS_OK`, or `ERR_INTERNAL` with that cause, so a handler returns it directly. The harness pins
it (`ava1_rpc_text_answers_ok_when_it_fits_and_internal_when_truncated`) and the server answers
`ERR_INTERNAL` ("reply exceeds the 256 KiB RPC cap") for a handler that claims more than the cap.

Threads. The payload runs a management RPC (method 4 and up, except the data plane's 16-19) on a worker
with a 512 KiB stack (`AVA1_MGMT_STACK`, the FTX2 management thread's size); every other RPC keeps the
256 KiB `AVA1_THREAD_STACK`. Before each handler the worker re-applies the credential elevation and sets the
in-flight frame marker to the handler's legacy FTX2 frame number (the crash breadcrumb), and clears it
after. A ported handler still keeps stack buffers small: **no stack array of 16 KiB or more reachable from
a table handler**, enforced by `payload/tools/mgmt_audit.py stack` (run by the `ava1-ctest` test
`c_mgmt_handlers_never_read_the_socket_and_keep_small_stacks`), and the handler must not read `client_fd`
(it is called with -1 behind a capture sink, `payload/src/mgmt_rpc.c`). `mgmt_audit.py report` lists every
array of 2 KiB or more per handler (it is a tripwire, not a proof: it reads C text, follows calls and function-pointer arguments by name, sizes struct elements as a lower bound and unknown element types at 8 bytes, and cannot see calls through tables or dlsym; `mgmt_audit.py selftest` pins the shapes it must catch); Tasks 5 and 7 must heap-allocate the 64 KiB buffers it shows in
`handle_crc32_file`, `handle_shell_builtin` and `copy_file` before routing those handlers.

7.4 RPC limits. A session has at most **8** requests in flight; the ninth answers `ERR_BUSY`
(earlier drafts said 4). A request body is at most **56 KiB** and a reply body at most **256 KiB**,
both enforced by the server (`RPC_REQUEST_MAX`, `RPC_REPLY_MAX`): a larger request answers
`ERR_PROTOCOL` with the cause `request exceeds the 56 KiB RPC cap` and the session continues; a
handler that returns more than 256 KiB is answered `ERR_INTERNAL` with the cause `reply exceeds the
256 KiB RPC cap` rather than clipped. The control connection's frame cap is 64 KiB while a session
is being set up; once the handshake is done the client accepts replies up to
`RPC_REPLY_MAX + RPC_FRAME_SLACK` (1 KiB for status, length, extension count and the AEAD tag). The
worst case per session is 8 × 256 KiB = 2 MiB of reply buffers on the node (heap, per call). A
method whose reply can exceed the cap takes `offset` and `limit` and sets `more`; no management
method may return a larger body. `MGMT_METHODS.md` lists today's largest reply of every method and
says which fit and which page.

The engine side (a Task 4 requirement, not current behaviour): the engine's gate holds 6 permits per
console and reserves the other 2 for `node.status`, `job.status` and `job.cancel`, so a flood of
slow calls never hides a cancel or liveness; it retries `ERR_BUSY` with backoff (3 tries) and never
reports it as "payload failed".

7.5 Chunked and bounded filesystem calls.

`fs.read` (`FsRead{path, offset, len, flags}` -> `FsReadResult{data, eof}`): `len` is at most
`FS_READ_MAX = RPC_REPLY_MAX - 16 = 262,128` bytes (the reply is `data + 7`). A shorter reply with
`eof = 1` means the end of the file; `eof = 0` with fewer bytes than asked means the node chose a
short read, and the caller continues at `offset + data.len()`. A caller that needs more than
`FS_READ_MAX` (FTX2 allowed 2 MiB per call) loops until `eof` or the byte count it wanted, and the
core wrapper `fs_read_with_timeout` does that for every caller. Callers that can ask for more than
the cap: `ps5upload-engine/src/lib.rs:3974`, `ps5upload-engine/src/fakelibs_api.rs:423`,
`ps5upload-core/src/fs_ops.rs:1703`, `ps5upload-core/src/smp_image_rw.rs:174`,
`ps5upload-core/src/smp_checkout.rs:185`. Existence tests by 1-byte `FsRead` that become `fs.stat`
(Task 4): `lib.rs:4033`, `smp_checkout.rs:456`, `smp_image_rw.rs:229`, `fakelibs_api.rs:390` and
`fakelibs_api.rs:594`.

`fs.write` (`FsWrite{path, offset, flags, data}`, ext `mode`): FTX2 wrote up to 256 KiB atomically,
and the request cap is 56 KiB, so a larger file is written in chunks of at most
`FSW_CHUNK_MAX = 48 KiB` (49,152) of `data` (the rest of the request is the path, the header and
the extension). Flags: `FSW_APPEND` (write at the end of the temporary file, `offset` ignored),
`FSW_AT_OFFSET` (write at `offset`; a missing temporary file is created empty), `FSW_COMMIT`
(after this chunk: fsync the temporary file, then `rename` it over `path`), `FSW_CREATE` (at commit
fail with `ERR_EXISTS` when `path` exists) and `FSW_OVERWRITE` (replace it; the default when neither
is set; both set is `ERR_PROTOCOL`). Neither `FSW_APPEND` nor `FSW_AT_OFFSET` means "the whole file in
one call": `offset` must be 0, the file is written to the temporary file and committed in the same
call, exactly FTX2's atomic small write (`COMMIT` is implied). Chunked protocol: the temporary file
is `<path>.ps5upload.tmp` in the same directory as `path` (so the commit rename never crosses a
device, with the `st_dev` guard of `fs.rename`); the caller sends chunks with `FSW_AT_OFFSET`
(or `FSW_APPEND`) in order, the last one also carrying `FSW_COMMIT`. A caller that gives up
deletes the temporary file (`fs.rename` is not needed; `job.run` DELETE removes it). A chunk with `FSW_AT_OFFSET` at offset 0 truncates an abandoned temporary file first, so a retry
starts clean. (`FSW_APPEND` never truncates: it has no offset to say "first", so an `FSW_APPEND` writer starts from a temporary file that does not exist; the engine uses `FSW_AT_OFFSET`.) `mode` (ext 1, the
permission bits applied at commit; absent = 0644) is optional. Callers that need chunking because
they write more than 48 KiB: `ps5upload-core/src/cheats.rs:701`, `ps5upload-core/src/profile.rs:664`,
`ps5upload-core/src/smp_image_rw.rs:158` (the others, `smp_checkout.rs` and `smp_image_rw.rs:305/339`,
write small state files). The core wrapper `fs_write_bytes` chunks transparently.

Other filesystem methods, as built (`payload/src/mgmt_fs.c`, host-tested): `fs.list` pages by `offset`/`limit`
(`limit` 0 = 256, at most 256; `more` = entries remain; names that are not valid UTF-8 are listed with `?` for
their high bytes; `total_scanned` counts what the walk passed). `fs.stat` takes any absolute path without a `..`
component (the policy of `fs.list`, not of `fs.read`; see "Scope of `fs.stat` and `fs.list`" below), follows a link (`kind` is `link` only for a dangling
one), and answers `ERR_IO` with `fs_stat_failed_errno_<n>` for an absent path. `fs.mkdir` honours `mode`
(applied to the new directory despite the umask; intermediate directories get 0777) and `parents` (0: a missing
parent is `fs_mkdir_failed`); an existing directory succeeds, an existing non-directory is `ERR_EXISTS`.
`fs.rename` with `overwrite = 0` refuses an existing destination (`ERR_EXISTS`, `fs_move_exists`); with
`overwrite = 1` it replaces, as FTX2's move did. `fs.read` reads at most `FS_READ_MAX` and loops internally
until the ask or the end, so a reply shorter than the ask always has `eof = 1`. `log.klog` sets `more` when the
read filled the whole ask; `log.syslog` returns the newest `RPC_TEXT_MAX` bytes (or `max_bytes`) and sets `more`
when older text was cut. A handler whose failure carries data the caller reads (`net.reach`, `fs.mount_pkg`,
`fs.mount_lwfs`) answers an error status whose cause is the whole `{"ok":false,...}` body (up to 1 KiB), which
the engine's `call_legacy_ok` hands back as the reply it parses.

Scope of `fs.stat` and `fs.list` (review 006, checklist D; decided: not narrowed). Both answer for any
absolute, `..`-free path, as the FTX2 handlers did, because the product needs it: the Volumes and file
browsers list `/`, `/mnt/*`, `/user` and `/system_data`, and existence probes (installed titles, SMP
overlays, backport libraries) ask about paths outside every writable root. What a paired peer learns is
metadata only: names, kind, size, mtime, mode, device. Never contents (`fs.read` and the data plane keep
their own read policy), and the trust store's contents are refused by the read and write policies (S2), so no key
material or peer list is reachable (its name and size are visible, like any other file's). A peer that is paired can already upload, delete, rename, launch and
read through `fs.read`'s allowlist, so metadata of the rest adds no capability. Narrowing these two
would break the browsers for no gain; the decision is to revisit it only if a read-only or guest pairing
tier is ever added (then `fs.list`/`fs.stat` would take the read policy).

Typed bodies decoded by the adapters: `NodeStatus.ucred_elevated` is a `u8` on the wire; the engine
adapter restores the JSON boolean the client reads (`true`/`false`) and rebuilds the legacy
`/api/ps5/status` object (Tasks 3 and 4). `NodeStatus.prior_instance` is one of `clean`,
`killed_externally`, `wedged`, `stale` or `replaced` (the values of `instance_verdict_name`).
`FsEntry.kind` and `FsStat.kind` are `ENTRY_FILE` (0), `ENTRY_DIR` (1), `ENTRY_LINK` (2, a symbolic
link, not followed), `ENTRY_OTHER` (3, a device, socket or fifo) or `ENTRY_UNKNOWN` (4, the node could
not stat the entry; FTX2 said `"other"`). `FsListResult` carries no `path` and no returned-entry
count (FTX2's reply had both); the adapter reconstructs `path` from the request and the count from
`entries.len()`.

## 8. Limits
A server accepts at most 64 connections, 12 from one source address, and 16
sessions (2 of them unconfirmed, §5); past any of these it sends
`Error(ERR_BUSY)` and closes. The accept loop never stops on an accept error.

Job admission is bounded as well: a receiver has at most 32 jobs open (the console's job table;
the engine's host counts per session). A `JobOpen` past it is answered `JobOpenAck{ERR_BUSY}`
and a `Resume` `JobMap{ERR_BUSY}`; the session and the admitted jobs are untouched and the
sender retries (review 006, checklist T).

One session per device: when a client completes a handshake (message 3 proves its
key) while that key still has a session, the older session ends at once — its
control connection and lanes are closed and their per-address counts given back
before the new session's limits are checked. A client reconnecting after its link
died silently is therefore never refused for its own dead connections.

The key is the identity, not the process: two engines (two processes, two computers, a
desktop app and a Docker engine) that share one identity file are one device to the
console, and each new handshake ends the other's session and its jobs' lanes. They keep
evicting each other, and each sees its session end for no visible reason. Every engine
therefore needs its own identity (its own data directory). A client can recognise the
condition but not prove it: the session simply ends. The engine logs a warning naming
this cause when a console's session ends on its own 3 times within 120 s (at most once
per window per console).

## 9. Data lanes
A client opens lane n (1..=8) by connecting and sending, unsealed,
`Join{session_id, lane_id, client_nonce, tag}` with a fresh random client_nonce
and the Join tag of §4.5. The server refuses (`Error(ERR_BAD_JOIN)`) an unknown
session, a lane id outside 1..=8, a wrong tag, or a client_nonce it has seen in
this session's last 64 joins; and `ERR_NOT_PAIRED` while the session is not
paired. Otherwise it draws a fresh random server_nonce and sends, unsealed,
`JoinAck{lane_id, server_nonce, tag}` (JoinAck tag, §4.5); the client checks the
tag. Both sides then seal everything after with lane_key(c2s|s2c, n, client_nonce,
server_nonce) (§4.3), counters from 0. The client sends a sealed Ping on the lane
as soon as it has checked the JoinAck. A join of a lane id that is still live
supersedes the older connection, but only once the new connection's first sealed
frame has opened under the new lane key (within the handshake timeout): a replayed
Join cannot prove the key and leaves the live lane alone. Lanes end with their
session.
A lane carries heartbeats and, on a node with `CAP_DATA_PLANE`, the lane data frames `Chunk`
and `Bundle` (§12) and `Error`; any other frame without the IGNORABLE flag is answered
`Error(ERR_PROTOCOL)` and closes the lane.

Lane sockets (informative): both ends ask for 4 MiB `SO_RCVBUF` and `SO_SNDBUF` on every lane, the
client before it connects and the console on accept, before the first read, and take whatever the
kernel grants. A lane thread that is busy opening a 15 MiB frame for 10-20 ms then does not close
the sender's TCP window. A receiver may keep a small pool of frame buffers for chunks whose size is a class (1, 4, 8, 15 or 16 MiB; every other size is allocated exactly, so memory in use never exceeds what the credit window counted, and idle pooled memory never exceeds the admit budget) so a lane does not allocate and fault in a fresh 15 MiB block per frame; this is not visible on the wire.

## 10. Version 1 scope
Version 1 is what this document specifies; the sections below say what that is and what it is
not. Unknown frame types on a control connection are a protocol error; new frame types require a
version bump or a negotiated `caps` bit.

In version 1:
- Project 1 (§1–§9): framing, codecs, keys, handshake, pairing, the trust slot and launch
  token, heartbeats, RPC `node.info` (and `pairing.open`, `crypto.bench`).
- Project 2 (§11–§16): jobs and manifests, chunks and bundles on lanes, credit, verification
  groups and outboards, journals, resume, staging, apply and the governor; uploads
  (folders, single files, file lists, zip archives read as sources, local and NAS sources),
  downloads (to a folder or to a zip), console-local copy and move, and PS5 → PS5 through
  an engine relay (the engine downloads from one console while it uploads to the other,
  with a bounded in-memory hand-off); the data RPCs `job.copy`, `job.status`, `job.cancel`
  and `disk.calibrate`.

Not in version 1, each with its reason:
- Direct PS5 → PS5 (tickets, Ed25519 signing, cross-network encryption): the relay is the only
  PS5 → PS5 path; direct transfer needs a trust model between two consoles. PS5 → PS5 has
  engine support but no UI wiring (project 3).
- Engine ↔ engine sharing: `host::FolderHost` exists as the receiving half, the sharing
  policy and the feature are deferred.
- Zstd bundles and small-file deduplication: ruled out of project 2.
- Zip entries larger than 256 MiB (`ZIP_MAX_ENTRY`) as AVA1 sources: entries are inflated
  on demand and there is no streaming entry reader yet, so an archive with a larger entry stays
  on FTX2.
- 7z and RAR sources: their decoders are forward-only, so there is no random-access `Source`
  for them; they stay on FTX2 until project 3.
- Full re-verification of durable groups on resume: §13.4 re-hashes only the last durable
  batch of each partial file and trusts older groups to the journal.
- Sources of unknown length: every file's size must be known when the manifest is built.
- Auto-tuning of the small/large cutoff (the design spec's 64 KiB–4 MiB range): the cutoff
  is the protocol constant `LARGE_CUTOFF`, §12.2.
- Resuming a Deflate zip download: a deflate stream cannot be continued, so the optional
  Deflate archive restarts with a fresh job per attempt (progress stays monotonic) and says
  so. The default archive is Stored and does resume (below).
- Resuming a download whose remote manifest changed: the engine restarts that job.

### 10.1 Zip downloads resume

A download into a `.zip` writes Stored (uncompressed) entries and resumes mid-entry on a
reconnect, within one engine run (the job id is reused, as for a download to a folder). No
wire change and no journal record: the receiver is `JF_ORDERED` and its journal already holds the
durable files and the durable prefix of the file in flight.

- Layout: every entry is `local header ‖ data ‖ data descriptor` (zip64 throughout), in manifest
  order, then the empty files, then the central directory and the zip64 end records. Because the
  manifest carries every size, entry `i` starts at the sum of the entries before it: offsets are
  recomputed on resume, never recorded.
- Resume: after the §13.4 re-check and before the map is sent, the receiver passes the journal's
  state (finished files, partial prefix) to the sink (`Sink::position`). The sink verifies each
  finished entry's header and data descriptor (the descriptor holds the CRC-32), rebuilds the
  in-flight entry's CRC-32 by reading its durable bytes back, truncates the archive to
  `data offset + durable bytes` and continues. The sender, told by the map, sends the rest.
- The sink's fsync runs inside the receiver's batch (data sync, journal append, Durable), so the
  journal is never ahead of the archive. A sink that cannot honour the journal (a missing or
  altered archive) makes the receiver drop the journal and start the job over.
- A file the receiver asks to have sent again (a verify mismatch) still restarts the archive.
- Deflate remains available as an option (`ZipCompression::Deflate`); it cannot resume.
- A same-drive `fs.move` over AVA1 is `fs.rename` (§7.3), with the `st_dev` guard; a cross-mount
  move (copy, verify, delete) is an AVA1 job.
- Typed bodies for the management text methods (§7.3): the filesystem, node and job methods are
  typed; the rest carry `MgmtText` and are typed method by method after the cutover.

## 11. Jobs and manifest

11.1 Every transfer is a job with a 16-byte `job_id`, chosen by the node that opens it
(the engine uses the HTTP API's `tx_id`). A job is bound to the static key of the peer
that opened it; only that key may resume or cancel it. Every data-plane message
(types 0x20–0x3F) has `job_id` as its first field, so a router reads it from body[0..16].

11.2 Paths in a manifest are relative to the job root: UTF-8, '/'-separated, at most
1024 bytes, no empty, "." or ".." component, no NUL, no leading '/'. A receiver refuses
a manifest with any other path (`ERR_PATH`) before touching the filesystem.

11.3 Opening (`kind` as seen by the node that receives `JobOpen`):
- `JOB_UPLOAD`: opener → `JobOpen`; receiver → `JobOpenAck{credit, staged}`; opener →
  `ManifestPage`* → `ManifestEnd`; receiver → `JobMap`; then data.
- `JOB_DOWNLOAD`: opener → `JobOpen{root = source, ext credit}`; the other node becomes
  the sender: `JobOpenAck`, `ManifestPage`* → `ManifestEnd`; opener → `JobMap`; then data.
- `JOB_COPY` runs on one node (`job.copy` RPC, §7).
Entries are numbered 0.. in manifest order (`file_id`); directories are entries with
`kind = ENTRY_DIR`. `manifest_hash` = BLAKE3 over, for every entry in order,
`u32le(len) ‖ encoding of the entry without its ext`.

11.4 Policies: `replace` (send everything not in the map), `skip-existing` (the receiver
marks a file done when a file of the same size and mtime seconds exists), `verify` (the
sender puts each file's root in `ext root`; the receiver marks a file done when an
existing file of the same size hashes to it).

A sender that wants "skip files the console already has" picks the policy from its source:
when every file reports a real mtime (local disk; SMB, FTP and SFTP servers) it uses
`skip-existing`; when any file's mtime is unknown (`mtime = 0` in the manifest — a backend
that reports none) it uses `verify` for the whole job, computing each file's root before
the manifest is sent. `verify` costs one read of the source plus a hash of each existing
console file of the right size, but is correct without an mtime; the sender never guesses
an mtime. The engine's "safe" resume mode always uses `verify`. The known weakness of
`skip-existing`: a file whose content changed but whose size and mtime did not is skipped.
A directory's mtime is always 0 and is ignored by both policies.

11.5 A `JobOpen` for a job the receiver already knows is a resume: the receiver matches
the new manifest against its stored one by path, keeps the progress of entries whose
size and mtime are unchanged, and restarts the others (never splicing old and new bytes).
`Resume{job_id, manifest_hash}` is the fast path when the sender still holds the same
manifest: the receiver answers `JobMap`, or `JobMap{status = ERR_UNKNOWN_JOB}` and the
sender falls back to `JobOpen`. A map larger than one control frame is sent as several
`JobMap` pages; `last = 1` marks the final one. `Durable` is never paged: each is complete.
Engines reopen with `JobOpen` after any interruption; `Resume` is optional for senders that
keep their manifest and credit state. A receiver answers `Resume` with the `JobMap` of a parked job
of the same peer key whose stored manifest has that hash, else `JobMap{status = ERR_UNKNOWN_JOB}` —
never silence.
The final `JobMap` page may carry the extension `held`: the bytes the receiver's drive already
holds for the job's unfinished large files (the allocated blocks of their part files, at most
each file's size; a part file is preallocated whole, so its undurable tail is on the drive
already). It is advisory and only ever a credit: a sender that checks free space before
sending subtracts it, with the files in place and the durable ranges, from what the job still
needs. Absent means none is claimed, and a sender never credits more than the receiver said.
Credit restarts after any interruption and nothing outstanding carries across a reconnect: the
grant in a `JobOpenAck` is an absolute number that sets the sender's window (a `Credit` that
arrives later adds to it), and a `Resume` restarts the window the same way — the receiver resets
the job's outstanding-credit count to its current grant and re-sends that grant as `Credit`. (Conformance: the console receiver sends the grant minus what the job still holds, the engine host its full
grant; the sender's window is exactly that number either way. Tests: `wire_upload.rs`
`a_resume_after_a_dropped_session_sends_credit_and_the_job_completes`, `data_rust.rs` the Resume test.)

11.6 Staging: when the job root does not exist, the receiver writes the whole tree under
`<root>.ava-part/` and, after the last file, renames it to `<root>` (same parent, `st_dev`
checked; a device that cannot be read is refused with `ERR_IO`, never taken as the same). When the root exists, files are written in place; large files through
`<name>.ava-part` and a same-directory rename. `JF_SINGLE_FILE` writes `<root>.ava-part`. A staging
receiver takes `<root>` with `mkdir` before it journals the job (an existing `<root>` then
refuses it, `ERR_EXISTS`) and records that in `JnlOpen.staged` bit 1, so on resume the empty
`<root>` is its own; the final rename replaces only that empty folder (not empty: `ERR_EXISTS`).

11.7 Limits and lifetime. A node lists at most 32 jobs; past that, or when it has no buffer budget
left for another job (§12.4), a `JobOpen` is answered `ERR_BUSY`. A manifest has at most 4,000,000
entries (a receiver refuses growth past that), and its pages are sized to fit a control frame (the
console writes at most 60 KiB per page). At most one running job writes a destination: a `JobOpen`
or `job.copy` whose root is, or lies inside or around, the root of another job that has not ended
(and, for a move, its source) is answered `ERR_BUSY`; parked jobs count, because they can resume.
When a session ends its jobs are parked, not ended: they stay listed, detached, for 10 minutes
and then leave the table; their journal stays on disk (§14.3), so a later `JobOpen` resumes them.
A finished upload or download leaves the table 10 s after its session lets go of it; a finished local job
(`job.copy`) stays listed for the full park age so `job.status` can still answer. Either peer ends a
job with `JobCancel{job_id, reason}`, where `reason` is the `ERR_*` code the ender wants reported
(`ERR_CANCELLED` for a user's cancel, `ERR_IO` or `ERR_VERIFY` when a sender's source fails); the
receiver then ends the job with that status in `JobDone` and keeps the journal.

11.8 Job flags (`JobOpen.flags`, `JobCopy.flags`, `JnlOpen.flags`). A receiver answers
`ERR_PROTOCOL` to a flag it does not know.

| flag | value | meaning |
|------|-------|---------|
| `JF_SINGLE_FILE` | 1 | the root is a file path and the manifest has one file entry; the part file is `<root>.ava-part` (§11.6). On `job.copy` the node derives it from the source, and passing it for a directory source is `ERR_PROTOCOL` |
| `JF_ORDERED` | 2 | the receiver consumes the files in manifest order (a download written into a zip, a relay); the sender then reads with one reader |
| `JF_UNSAFE_READ` | 4 | a sender may read outside the roots its read policy allows (system files); the engine sets it only for a download the user marked unsafe |
| `JF_MOVE` | 8 | `job.copy`: delete each source file after its destination is durable (§12.6, §15.5) |
| `JF_OVERWRITE` | 16 | `job.copy`: replace destination files that already exist; unset, an existing destination root is refused with `ERR_EXISTS`. Not valid on `JobOpen` |

11.9 Messages. All are data-plane messages (§11.1). "Sender" and "receiver" are the roles of the two
peers for the job (§11.3), not who opened it.

| type | message | direction | where |
|------|---------|-----------|-------|
| 0x20 | `JobOpen` | opener → peer | control |
| 0x21 | `JobOpenAck{status, credit, staged, workers}` | answerer → opener | control |
| 0x22, 0x23 | `ManifestPage`, `ManifestEnd{files, bytes, manifest_hash}` | sender → receiver | control |
| 0x24 | `JobMap` | receiver → sender | control |
| 0x25 | `Resume` | sender → receiver | control |
| 0x26, 0x27 | `Chunk`, `Bundle` | sender → receiver | a lane |
| 0x28 | `Received{lane, seq}` | receiver → sender | control |
| 0x29 | `Credit` | receiver → sender | control |
| 0x2A | `Durable` | receiver → sender | control |
| 0x2B | `FileRoot` | sender → receiver | control |
| 0x2C | `FileRetry{file_id, reason}` | receiver → sender | control |
| 0x2D | `Status` | receiver → sender (IGNORABLE) | control |
| 0x2E | `JobDone{status, files, bytes}` | receiver → sender | control |
| 0x2F | `JobCancel` | either | control |

`FileRetry.reason` is `RETRY_VERIFY` (1, the root did not match), `RETRY_IO` (2, the receiver lost
the part file or outboard) or `RETRY_CHANGED` (3, the record's length disagrees with the manifest).

## 12. Data frames and credit

12.1 `Chunk` and `Bundle` travel on lanes; everything else on the control connection.
The header `channel` of a lane data frame is the sender's per-job sequence number.

12.2 `Chunk.offset` is a multiple of 1 MiB (one verification group, §13); its length is
a multiple of 1 MiB unless the chunk ends the file. A file is a *large* file when its
size is at least `LARGE_CUTOFF` (256 KiB), a protocol constant both sides use (`JobOpen`
carries no cutoff); smaller files travel whole, as `BundleRecord`s. A receiver ends the job
with `ERR_PROTOCOL` on a `Chunk` for a small file or a `BundleRecord` for a large one. A piece is
never larger than the credit the receiver granted (§12.4): the sender caps a piece at the smaller of
the chunk size and the granted window, floored to whole verification groups, and a grant below
one group fails the job loudly instead of stalling.

12.3 `Received{lane, seq}` is sent as soon as the receiver has a lane frame in memory,
before any disk work. A sender requeues, on any lane, the frames of a lane that closed
before they were `Received`. Applying a frame twice is harmless. A lane's death releases
window credit only for frames that provably never left the sender: frames the lane's writer
never dequeued (still queued when the writer is confirmed dead) are dropped and their bytes
returned to the window. A frame the writer may have put on the wire stays charged until the
receiver accounts for it (its `Credit` after the apply) or the job ends — releasing those
would let the sender spend the same window twice, and the receiver's `ERR_CREDIT` (12.4)
would fail a healthy job.

12.4 Credit: `JobOpenAck.credit` (uploads) or `JobOpen.ext credit` (downloads) is the
number of lane-frame body bytes the sender may have outstanding — the window counts the whole
body, so a `Chunk` costs its data plus 34 bytes of framing; `Credit{bytes}` returns space as the
receiver frees buffers. A version 1 receiver grants 64 MiB, and never less than 8 MiB: a node whose
global buffer budget cannot cover 8 MiB refuses the job with `ERR_BUSY`. A receiver that sees a
lane frame exceed the credit still outstanding sends a sealed `Error{ERR_CREDIT}` on the offending
lane and closes that lane only: the session, the job and its other lanes go on. Nothing of the frame is
buffered or acknowledged, and the sender observes ERR_CREDIT on that lane; it requeues the frames the
lane never had `Received`, as for any dead lane (§12.3). A sender never sends a piece larger than the credit already granted: pieces are sized at
read time to fit the window (whole verification groups, one group minimum; a file's final piece keeps
the whole-file rule), and a window that cannot hold one group — or whose smallest queued frame fits
no lane for 10 s with nothing sent, received or credited — ends the job (`ERR_PROTOCOL`) instead of
stalling. The 10 s applies while the receiver holds no window bytes (a window that can never fit the
frame). While it still holds some — a drive in a long flush has not yet returned its `Credit` — the
receiver is slow, not dead, and the sender waits (a dead one is caught by §6 liveness), failing only
after 10 minutes without progress. A lane's death does not refund window credit that the receiver has not accounted for
(§12.3): its un-received frames are requeued with their bytes still charged, and the charge is
released only when the receiver accounts for them (its `Credit` after the apply) or the job ends.

12.5 Per lane, the sender keeps at most `max(chunk size, lane rate × 2 s)` bytes sent
and not yet `Received`.

12.6 `Durable{files, ranges}` follows the order: data synced → journal appended and
synced → `Durable`. What "data synced" means for a small file is the receiver's choice, but what
the journal proves is not: a file reported durable can be reproduced by the receiver alone, either
because its bytes and name are on stable storage in place, or (durable-by-log, §15.7) because a
log record that holds them is, and a durable journal record names it. `JobDone` follows the last durable commit (and the staging rename).
A failure after every byte is durable (`ERR_EXISTS`, `ERR_CROSS_DEVICE` on the final
rename) is reported in `JobDone` and never causes a resend. A rename is durable only once its
directory is synced: the receiver fsyncs the parent directory after every commit or staging
rename, before it journals that commit. Likewise for new names: before a batch is journaled, every directory
that gained a file in it is synced once, after the file data. Where fsync does not reach stable
storage (macOS), a receiver flushes the drive's cache once per batch after the per-file fsyncs.

12.7 Download pipeline (receiver on the engine, informative; §12.6 is what binds). The pipeline is
tuned so a download is bound by the wire, not by the receiver: (a) a job with nothing to resume sends
its (empty) `JobMap` before the journal and the destination folders are made durable, so the sender's
turnaround to its first data frame overlaps that setup; (b) the files of one `Bundle` are root-checked
and written on a blocking task of their own, several bundles at once, while the receiver goes on
draining the inbox; a bundle's `Credit` is returned when its files are written, not when its frame
arrived; (c) the sync batch is due every 250 ms and at once when every file of the job has been
written (waiting for the next tick there only adds latency). None of this weakens §12.6: a file is
reported durable, and `JobDone` sent, only after its data, its names and its journal record are
synced. A sender starts its reader and writers when the map arrives, not at the next 25 ms tick, and
its walk stats each path once (`lstat`; `stat` only for a symlink).

A transient `fsync` error does not fail the job at once. A receiver retries a failed `fsync` (file
data, directory, journal append, final commit sync) up to four more times, waiting 20, 60, 200 and
600 ms, when the error is one a drive can recover from: `EINTR`, `EAGAIN`, `EBUSY`, `ETIMEDOUT`,
`ENOENT`, `ENXIO`, `ENODEV`, in Sony's `0x8002xxxx` form too (the console reports a USB drive's
hiccup as `0x80020002`). `EIO` is never retried: it is the kernel saying the data did not reach the
drive, and an fsync that "succeeds" afterwards proves nothing (the kernel may have dropped the dirty
pages that failed). A retry that finally succeeds is not trusted alone either: the receiver reads
back what that fsync covered — each small file is re-read and its BLAKE3 compared with the root it
arrived with, each large-file range of the batch is re-read and its group chaining values compared
with the outboard's — and a mismatch ends the job with `ERR_IO` before anything is journaled or
acknowledged. Nothing is acknowledged while a retry is pending; every retry is logged
(`[ava1] fsync failed (errno 0x80020002), retry 1 of 4`). Exhausted retries end the job with
`ERR_IO` ("fsync failed"), as before.

The console-local copy and move (§15.5) use the same standard in memory: a copy is a receiver job
fed by an in-process reader, with no read-back of the destination; a move deletes a source file only
after every destination group of that file is verified in memory, the file and its directory are
fsynced and the destination's `Done` is journaled; a copy that fails deletes nothing.

12.8 Progress watchdog (review 006 #2). A Ping is a byte, so §6 cannot see a sender that heartbeats
while its data pump is wedged (a source read stuck on a network share): such a job would stay open
with no byte of file data moving. A receiver therefore ends a job that has made no progress for
the progress deadline while the sender still owes bytes: a file is neither written nor in a sync
batch, the receiver has nothing of its own in flight, and the sender is attached. Progress is a
lane data frame admitted, a `FileRoot`, a finished bundle write, or a sync batch that made
something durable. The deadline is 3 x `dead_after` (36 s at the default 12 s); a job that resumed
whose journal held finished or partial files when it opened (decided once, at open, or at a reattach that finds durable work) gets 15 minutes,
since its sender may hash or skip what is durable for a long time without producing a frame. The clock runs only while the sender is attached
(a lane is up on the engine, a session on the console), and any finished sync batch that had work counts as progress. A slow drive never counts: any queued or running write, or a
running sync batch, holds the clock. The engine's receiver sends `JobCancel{reason: ERR_STALLED}`
and fails the job (the engine's session retry resumes it); the console ends it with `JobDone`
status `ERR_STALLED` and keeps the journal, as for every console-side failure. The sender
observes the code like any receiver-ended job and may resume.

## 13. Verification

13.1 A file's root is its standard BLAKE3 hash. Files are hashed in groups of 1 MiB
(`GROUP_SHIFT` = 20): group i covers bytes [i·2^20, (i+1)·2^20). For a file of two or more
groups, each group's chaining value (BLAKE3 `finalize_non_root` of that subtree) is
computed where its bytes are, and the root is merged from the group CVs along BLAKE3's
tree (left subtree = the largest power of two of groups strictly less than the count);
every merge is a non-root parent compression except the last, whose compression carries
BLAKE3's ROOT flag. A file of zero or one group: root = BLAKE3(bytes).

13.2 Senders compute the root while reading. Small files carry it in their
`BundleRecord`; large files send `FileRoot` once their last group is read (or, on a
resume, once all group CVs are known from the sender's outboard).

13.3 Receivers compute each group's CV from the bytes they write and store it in an
outboard (32 bytes per group, in the job directory). At commit the root merged from the
outboard must equal the sender's root; otherwise the file is reset and `FileRetry` sent.

13.4 Resume verification: before answering a resumed job's map, the receiver re-hashes the groups
of each partial file that its last durable batch covers and compares them with the outboard; a
mismatch drops those ranges from the map. The console re-hashes only that batch and trusts older
durable groups to the journal — version 1 never re-verifies every durable group of a console
partial file. A receiver may check more: the engine's receiver re-hashes every durable group of
every partial file and resets a file on any mismatch. The verify policy re-hashes whole files.

## 14. Journal and resume

14.1 The journal: `<job dir>/journal` = magic `AVA1JNL1` (8 bytes), then records
`u32le(len) ‖ u8 kind ‖ body ‖ u32le(crc32c(kind ‖ body))`, `len` = 1 + body length. Kinds:
1 `JnlOpen`, 2 `JnlBatch`, 3 `JnlReset`, 4 `JnlSnapshot`, 5 `JnlDone`, 6 `JnlSweep` (bodies are the
generated structs). Replay stops at the first record whose length runs past the file, whose CRC fails, or
that the visitor refuses; the file is truncated there before the next append. Every append is
followed by `fsync`. When the file passes 1 MiB it is compacted: `journal.tmp` =
magic + `JnlOpen` + `JnlSnapshot(current state)`, `fsync`, `rename` over `journal` (same
directory), `fsync` of the directory. The job's manifest is stored once as `<job dir>/manifest`:
exactly the content of a `records(ManifestEntry)` field (written as `manifest.tmp` → `fsync` →
rename).

14.2 State: a `JnlBatch` marks files done (dropping their ranges), adds durable ranges and
records roots; `JnlReset` forgets one file; `JnlSnapshot` replaces the whole state; `JnlDone`
records the job's final status. A `JnlBatch` carrying the pack extension (§15.7) also marks its
files *unswept*; a `JnlSweep` clears that for the files it lists; a `JnlSnapshot` carries the
unswept files (`unswept`: the `FileRun` item stream) and the pack ranges that still hold them
(`segments`: the `PackRef` item stream, one per segment, spanning its unswept records). Replay's
unswept set is "done, minus swept". The map answered for a resume is the replayed state (done files;
durable ranges of the others), after the check in §13.4.

14.3 Location: the console keeps job directories under `/data/ps5upload/ava/jobs/`, an engine
under `<data dir>/ava/jobs/`. Each holds `journal`, `manifest` and one `<file_id>.ob` outboard
per large file. A node removes a directory 7 days after its last write (directory or journal mtime), on
start and then once a day; the engine also sweeps `<data dir>/ava/send` (sender outboards) and never
touches the directory of a job that is running in the process. A job whose session ended is parked for 10 minutes first (§11.7).

## 15. Apply (receivers)

15.1 Directories first: every directory entry of the manifest is created before any file data
is applied, and the directories a batch created are fsynced before that batch is journaled. A
file whose parent is not a manifest entry still gets its parents created when it is opened.

15.2 Small files (a `BundleRecord` chunk): `open(O_WRONLY|O_CREAT|O_TRUNC|O_NOFOLLOW)` → write →
mode → mtime. The descriptor stays open until a sync batch covers the file; a duplicate record
for a file that is already durable or already pending is dropped without truncating it. A
record whose length disagrees with the manifest is answered with `FileRetry` reason
`RETRY_CHANGED`, and one whose BLAKE3 disagrees with the root it carries with `RETRY_VERIFY`;
the record is not applied and the job continues.

A receiver may instead take the durable-by-log path of §15.7 for these records.

15.3 Large files: `pwrite` at the chunk's offset into the part file (preallocated when new),
group CVs into the outboard. Bytes are durable after a sync batch. When every byte is durable
and the root merged from the outboard equals the sender's `FileRoot`, the file commits: mode →
truncate to size → mtime → fsync → a same-device check (§12.6) → a rename in the same directory
→ an fsync of that directory. The part file of a single-file job is `<root>.ava-part`; in a
merge it is `<path>.ava-part` beside the final file; in a staged tree (§15.5) it is the file's
final relative path inside the staging tree, so the file's own commit renames nothing. A part file or
outboard that is missing at commit time is a reset (`FileRetry` `RETRY_IO`), never a new empty
file.

A receiver opens and preallocates a new part file (so that ENOSPC is found first, and so that a
sparse file cannot collapse the drive's write rate under dirty-buffer throttling) without
holding the job's lock, and runs a commit on a worker rather than on the thread that batches
fsyncs. Journal appends from several commits and a batch are serialised, so their records land in
some order; each is independent, since a commit's record names one file and only follows that
file's rename and directory fsync. A journal compaction waits until no commit is between its
rename and its record. Where a drive's fsync of a batch takes longer than the credit window
holds data at the intake rate measured between batches, a receiver may fsync each chunk right
after writing it (the batch fsync then finds little to flush); this changes no ordering.

15.4 Sync batches: every 250 ms, or after `batch_max` small files (tuned 16–512, starting 256:
halved when a batch takes over 1.5 s, doubled when under 0.5 s) or 64 MiB of large-file bytes.
Data fsyncs run in parallel on the workers, then the new directories are fsynced (also in
parallel on the workers: each is an independent descriptor, and a directory is synced once per
batch however many files it gained), then the batch (`JnlBatch`) is appended and the durable
ranges are reported. A stop in the middle of a sync journals and acknowledges nothing.

(A logged batch, §15.7, replaces the small files' data and directory syncs with one fsync of the pack
log; the invariant below then applies to the sweep, which is where those directories are synced.)

The directory-sync invariant (the journal never runs ahead of a name): a `JnlBatch` naming a
file is appended only after every directory that gained an entry in that batch has returned from
`fsync`, and a prepare's new directories are likewise all synced before the map is sent. The
syncs are never deferred past the record; a crash with some of them still to do therefore leaves
no record naming those files, and the resumed job resends them. A receiver may run the syncs in
any order and on any threads, but not after the append.

15.5 Staging and merge: a new destination is staged — the tree is written to `<root>.ava-part`,
a sibling of the destination, while `<root>` itself is created as an empty lock folder; at the
end the finished tree is renamed over that empty folder (never over content). If the
destination appeared in the meantime the job ends `ERR_EXISTS` and the files stay in
`<root>.ava-part`. A merge into an existing folder refuses a manifest directory that is a
symbolic link or not a directory (`ERR_PATH`), and a file already where one must go ends the
job with `ERR_EXISTS`.

Version 1 has two walk modes and they are deliberately different. The sender/download mode
follows directory symlinks (the engine walks the same tree the same way); a symlink cycle is
an error, never a spin, and a dangling link — the link's target cannot be stat'd — is an error
in both modes. The console-local copy/move mode skips directory symlinks: the copy writes into
a namespace it must not be able to be led out of by the source tree. Both are contracts, not
bugs; the sender's descend behaviour is not normative for the copy path.

A copy (`job.copy`) is a receiver job whose sender is the in-process reader, so this section
applies to it unchanged. The job is owned by the peer key that issued it: `job.status` and
`job.cancel` from another key answer `ERR_UNKNOWN_JOB`. A `job.copy` for an id the node already
lists, from the same owner with the same parameters, answers with that job's `Status` instead of
starting a second one — which makes re-issuing after a lost connection safe; a failed job is retired
and restarted by a re-issue. `job.cancel` stops a job and unlists it (the journal stays), so a
caller never cancels a job whose terminal status it still needs. A finished local job stays listed for the park age, so `job.status`
keeps answering for it.

`job.copy` uses the source and destination paths, a stable job id, and flags including
`JF_OVERWRITE` and `JF_MOVE`. Without `JF_OVERWRITE`, an existing destination root is refused
with `ERR_EXISTS`; with it, colliding files are replaced and destination-only files remain.
A move deletes source entries only after the destination rename, parent directory sync and
successful Done journal append. It checks each source file against the manifest before unlinking
and reports any paths left behind as a failed job; `job.status` stays running (`state` 0, `current` =
"deleting source") during deletion, so `state` 1 is the only point at which a move is done.

15.6 Open files. The number of descriptors a process may hold open is a resource, and on the
console it is smaller than `RLIMIT_NOFILE` says: the measured ceiling on firmware 13.60 is about
619 open files while the limit reads about 13,952. At data-plane start a node therefore probes
(it opens `/dev/null` until the system refuses, bounded at 4096), and its open-file budget is the
smaller of the raised limit and the probed count, each less 128 descriptors held back for sockets
and the other services. Half of the budget is the share of pending small-file descriptors (§15.2) all
jobs together may hold, never fewer than 4; a worker that finds the share used up runs queued sync
work or waits, and the job thread syncs early so that the batch frees slots. A single job holds at
most 512 pending small files regardless of the budget. `disk.calibrate` (§16.10) works within the same
budget instead of opening every file at once.

15.7 Durable-by-log (small files; optional for a receiver, the console and the engine implement it).
A batch of N small files costs two fsyncs (the log, the journal) instead of N + D + 1; files are made
durable in place later, off the transfer's critical path.

Pack log. `<job dir>/pack.<n>` (n from 0, never reused within a job) = magic `AVA1PCK1` (8 bytes), then
records `u32le(len) ‖ u8 kind ‖ body ‖ u32le(crc32c(kind ‖ body))`, `len` = 1 + body length, appended
sequentially; kind 1 `PackFile` = a `BundleRecord` as received. A reader stops at the first record that runs
past the extent or fails its CRC. A record never spans segments; a segment is closed when the next record would
pass `PACK_SEGMENT` (64 MiB) and a new one starts. Segments are not preallocated (the log is fsynced every
batch, so dirty data never piles up the way it does under a part file). A new segment's directory entry is
synced when it is created, so a journal record may name it.

Receiver steps for a small file: validate as in 15.2; append the record to the log (the offset is taken and the
record written under one lock, so every record below a written one is written); create the file, write, mode,
mtime, close, with no fsync and no descriptor kept. A batch then (1) fsyncs the log segments written since the last
batch (a retried fsync is followed by reading the batch's records back and checking each CRC and root), (2) fsyncs
large-file data as today, (3) appends one `JnlBatch` per segment its records sit in, carrying the files as runs and
the pack extension (`pack_segment`, `pack_offset`, `pack_len`: the byte range of its records), fsynced, (4) marks the
files done and sends `Durable`. No directory is synced here.

Invariants. I1 A file reported `Durable` can be reproduced by the receiver alone: its bytes are in the file (swept)
or in a log record named by a durable journal record. I2 `Durable` and `JobDone` are never sent before the journal
record that proves I1 is fsynced. I3 A segment is deleted only after every file whose record it holds is swept and
the sweep is journaled durably. I4 Large files are unchanged. I5 Recovery is idempotent: re-making a file that exists
with the right bytes changes nothing, one with wrong bytes is rewritten.

Sweep. A worker with no other work (or one that would push the log past the cap) takes up to 64 done, unswept
files whose batch is at least `SWEEP_AGE` (3 s; every one when the job has ended) old: each is opened, re-made from its
log record if it is missing or the wrong size, and fsynced (a retried fsync re-makes it and syncs again); then each
distinct parent directory is fsynced; then `JnlSweep{files}` is appended and fsynced; only then does a segment whose
files are all swept get deleted. The sweep is the only place the directories of logged small files are synced.

Backpressure. The pack bytes of files not yet swept (pending ones included) are capped at `UNSWEPT_MAX` (256 MiB): a
worker that would pass it sweeps (or runs queued sync stripes) instead of writing, so the disk holds at most one cap
of duplicated bytes and throughput falls back to the per-file path's, never below it.

Recovery. On `JobOpen` of a known job, and for every job directory at helper start, the receiver replays the journal;
for each file done but unswept it locates the record in the pack ranges the replay kept, checks the record's root,
and re-makes the file when it is missing or its size or BLAKE3 differs, then sweeps. A file whose record cannot be found
or fails its CRC (a missing segment, a torn tail) is not done: a `JnlReset` forgets it and the sender resends it. A
successful recovery leaves no segment behind. Cost is bounded by the cap.

End of job and reporting. `JobDone` is sent once the final batch is journaled (I2), with ext `settling` = 1 while
files are still unswept; `Status` ext `unswept` carries the count (absent = 0) and the job stays listed, its sweep
running, until it reaches 0 (a session that ends does not stop it). A staged tree settles fully before its final rename
(the sweep addresses files by path), so `JobDone` carries no flag for it; so does a console copy or move, which must
not delete its source before what it copied is durable in place; merges and single files settle behind `JobDone`. A
sender that sees `settling` may keep the job open, reading `Status`, until `unswept` is 0 (an engine does, for up to 30 s,
to show "finishing on the console"); the report is true either way. A power cut inside the sweep's lag leaves the last files to be re-made by the next recovery.

Failure. A sweep that fails loses nothing: its files go back to the front of the queue and are tried again after a
backoff (100 ms doubling to 3.2 s). After five failures in a row the error is sticky: `Status` carries `code` =
`ERR_IO` and the reason in `current` until a sweep succeeds, and a job that is still running fails with it. A sender
that waits for `unswept` = 0 fails the upload on that `code` (the console cannot make its files durable); on its own
timeout or a cancel it ends the wait and reports the job with a warning, never as a clean success, because the
bytes are safe in the log. A drain (a staged tree's rename, a copy's end, a manifest change) retries a few times and
then fails; a drain cut by a stop writes nothing (no terminal `Done`). A manifest change is refused with `ERR_IO` when
the unswept files cannot be settled first (the sweep queue names ids, which the new manifest would renumber).

Nobody holding the job. A job directory that holds a pack log is never garbage-collected. A settling job that is
reaped (after the park age) is destroyed with its log on disk; the receiver's housekeeping takes such directories (and
those a crash left) through the same recovery as `JobOpen`, a bounded number per pass, every few seconds and at start.
Beside the per-job `UNSWEPT_MAX` there is a cap across jobs (512 MiB); it gates a job only past its own share (4 MiB),
and the bytes of a job in a sticky sweep error, or of a recovery pass, do not count against the others. Recovery runs on a
thread of its own (the reaper is never held up), takes a bounded number of directories per pass and moves on from one it
cannot settle, so a stuck directory never starves the rest; while it holds a job id a `JobOpen` for it is answered
`BUSY` (the sender retries with a bounded jittered backoff, honouring cancel, and then resumes; the engine does this for uploads, downloads and relays, and fails with reason `ava1_busy` once the bound, 12 tries by default, is spent). A log nobody could recover is garbage-collected a week after the normal
age, with a log line.

## 16. Governor

The sender and the receiver each keep one small control loop; both are pure functions of the
numbers they are fed, so both are tested against models rather than sockets.

- Start: 2 lanes, a 4 MiB chunk and a 1 MiB bundle target; receiver workers start at 4.
- Lanes: add one while the bottleneck is the network and the last addition raised throughput by
  ≥ 10 % (≥ 5 % while the rate is below 90 % of the best rate seen in the job); otherwise revert
  it and hold for 30 s. A tick with a lane death or requeue drops one
  lane (min 1) and halves the chunk. At most 8 lanes: on a link that scales past that the count
  simply stops growing.
- Chunk: 1–15 MiB in whole groups; halved on a stall, doubled after 10 stable ticks; never more
  than half a second of one lane's throughput (min 1 MiB). Lanes before chunk: while the network
  is the bottleneck, fewer than 4 lanes are open and no lane probe has failed yet, the chunk is
  not doubled past 4 MiB (a bigger frame lengthens every decrypt stall on the receiver); once a
  probe fails or 4 lanes are open, growth resumes. The sender can switch this policy off
  (`PS5UPLOAD_AVA1_LANES_FIRST=0`) to A/B it; it is on by default. 15 MiB, not 16: the frame cap (§2)
  counts the header and the MAC, which a 16 MiB body would not fit under.
- Benchmark pins (sender-local, never on the wire): `PS5UPLOAD_AVA1_LANES=n` holds the lane count
  at n (1-8) and `PS5UPLOAD_AVA1_CHUNK=m` holds the chunk at m MiB (1-15); a pinned value ignores
  stalls and the link. Out-of-range values are ignored. For measurement only.
- Bundle target: a quarter second of one lane's throughput, clamped to 256 KiB–15 MiB. It moves
  with that rate; the effect is that it grows while the network is the limit and shrinks when
  the rate falls (a receiver whose workers wait shows up as a receiver-reported bottleneck, §16.9
  — the target itself is not fed by worker pressure).
- In-flight cap per lane: `max(chunk, lane rate × 2 s)` — the same bound §12.5 states.
- Mixing check: once per job — after 3 warm-up ticks, if both classes still have work queued,
  the sender probes 5 s mixed, 5 s stream-only, 5 s bundle-only, then picks sequential if its
  estimated finish time is < 90 % of mixed. Sequential runs bundles first. The choice and reason
  go into `Status.sequential`. The probe never runs twice.
- Priority: beyond the bundle floor, the class with the longer estimated remaining time is
  preferred. The small/large cutoff is the protocol constant `LARGE_CUTOFF` (§12.2); it is not
  governed in project 2 — the design spec's 64 KiB–4 MiB auto-tuning is deferred.
- Receiver workers: every 2 s; add one while work is queued and the last addition raised
  files/s by ≥ 10 %; revert and hold 30 s otherwise; release one after 3 idle steps; range
  2–16, start 4.
- Bottleneck: source starved → `BN_SOURCE`; credit starved → the receiver's reported bottleneck
  (`BN_DISK`/`BN_WORKERS`), else `BN_CREDIT`; otherwise `BN_NETWORK`.

16.9 Status: the receiver sends `Status` (IGNORABLE) every 250 ms while a job is open: files
and bytes done, durable bytes, its own bottleneck (`BN_DISK` when workers are at their maximum
or adding one did not help, `BN_WORKERS` while it is still adding, `BN_NETWORK` when its queue
ran dry), workers, lanes, and whether it runs sequential. The engine shows the sender's
bottleneck, which already folds in the receiver's.

16.10 disk.calibrate: method 19 accepts `DiskCalibrate` and returns `DiskCalibrateResult` with
measurements at 1, 2, 4, 8 and 16 workers. The request allows at most 20,000 files of at most
1 MiB each, and `dir` must pass the node's write policy. The answer is a hint for the engine's
starting worker count, never a contract. The node deletes every file and directory it created.

## 17. Sequential sources (sender-local, no wire change)

A sender may read a source that can only be read forward (a 7z folder, a solid RAR): a
`SeqSource` (`engine/crates/ava1/src/seq.rs`). The receiver cannot tell: it sees ordinary
`Bundle`, `Chunk` and `FileRoot` frames, and the manifest (sorted, §11.3) is built from the
archive's headers before the job opens.

17.1 One decode thread replaces the random readers. It calls `SeqSource::pass`, which visits
entries in *decode* order, asks `want(path, size)` for each (`Keep::Skip`, `Keep::All` or
`Keep::Ranges(lacking)`) and feeds the wanted ones to an `EntrySink` (`begin`/`data`/`end`).
The thread maps the entry's path to its manifest id (the archive's order is unrelated to the
manifest's), cuts files below `LARGE_CUTOFF` into records and the rest into group-aligned
chunks exactly as the random readers do (§12.2, §13), and queues a `FileRoot` after a large
file's last chunk. It takes the same read-ahead permits, so a slow lane parks the decoder
and its memory stays bounded.

17.2 Resume (§14). Files the receiver reports done are `Skip`. The pass starts at the minimum
`restart_for(id)` over the unfinished files (7z: the folder's first entry; RAR non-solid: the
entry), so everything the receiver already has before that point is not decoded. A partly
durable large file is decoded from its start (a decoder cannot seek inside an entry); only the
groups the receiver lacks are sent, durable groups are hashed only when no persisted outboard
CV exists, and a file whose every CV is known needs no decoding at all. There are no decoder
checkpoints: a resume costs a decode of at most the restart folder's prefix.

17.3 `FileRetry` (§13) queues the file for a further pass over only the retried files
(`Keep::All`, their outboards dropped). A job makes at most 3 decoding passes (the first plus two retry passes; a pass that
has nothing to decode is not counted); a further retry request fails it. An entry that a pass never delivers, delivers twice, or delivers with a different size
than the manifest fails the job (the archive changed between listing and sending).

17.4 Cancellation. `pass` receives a flag raised by the job's cancel *or* by any other way the job ends (a lane,
protocol or receiver failure) and must poll it at least every 1 MiB of input, including while
skipping, and every `EntrySink` call fails
once the job is ending; `SeqSource::close` is called at teardown before the decode thread is
joined. The sender attributes the bottleneck itself: it is `BN_SOURCE` when lanes find nothing queued, except
in a tick in which the decode thread parked on its read-ahead budget (the budget is held by frames in flight,
so the lanes, not the source, are the limit then).

17.5 Entry metadata. The manifest carries each entry's own last-modified time as `mtime` (it is content, not
container metadata) where the format exposes it: 7z (the entry's FILETIME), zip (its DOS time, read as UTC) and
RAR (UnRAR's DOS time in the host's zone and 2 s resolution; a stamp in the future is read as absent, and
non-Unix hosts carry none). 0 means the archive has none. Directory mtimes are 0, and modes stay `0644` for files
and `0755` for directories: archives' Unix permission bits are not carried (zip's `unix_mode` excepted).

17.6 Refusals. A duplicate name, a path that is both a file and a directory (and, for RAR, two names that differ
only in case) fail the job terminally (`ava1_7z_unsupported`, `ava1_rar_unsupported`); only an unsupported 7z
coder method falls back to FTX2, since FTX2 has the same problem with the others (it writes both duplicates, or
hits the same decoder memory limit). A RAR's listing order is compared with its extraction order only when a
non-solid resume skips entries by position; any other pass binds entries by path.

## 18. Console to console

One console can send a job straight to another, without the bytes passing through the device
that asked (`engine/crates/ps5upload-ava1/src/c2c.rs`). Neither console is paired with the other;
the device paired with both (the engine) introduces them for one job.

18.1 `c2c.allow` (22), on the receiving console B: `C2cAllow{job_id, key, root}`, where `key` is the
sending console A's identity. B answers `C2cTicket{token}`: 16 random bytes. A ticket admits that
one key, showing that token, for that one job into that one `root`, acting for the device that
called `c2c.allow` (the job on B is that device's own, so it reads its `job.status` and can resume
it over any route). A ticket lasts 10 minutes from its last use; the same key and job again
replaces it. A node holds at most 8.

18.2 `c2c.send` (23), on A: `C2cSend{job_id, host, port, key, token, src, dest, flags}`. A dials
`host:port` (IPv4), runs the handshake as the client and refuses unless the static key the server
proves in message 2 is `key`. Its `ClientInfo` carries the ticket in ext tag 2 `token`. B admits a
key it does not know only with a live ticket for it: it answers `Welcome{knows_you = 1}` and marks
the session restricted. A joins up to 4 lanes (§9), sends `JobOpen{job_id, JOB_UPLOAD,
POLICY_REPLACE, JF_SINGLE_FILE when src is a file, root = dest}` and then runs the job as the
sender (§11–§13), exactly like a download's sender except that its window is the grant in B's
`JobOpenAck` and the job ends with B's `JobDone`. The reply is the sending job's `Status` once it
runs; a dial, handshake or ticket failure is `ERR_IO` with the reason as text. A's job is listed
under `job_id` and its `job.status` / `job.cancel` answer the device that called `c2c.send`. When
the session drops before the job ends, A fails the job (`ERR_IO`); a cancel on A sends B a
`JobCancel`. Either way B keeps its journal, so a later `c2c.send` or upload with the same
`job_id` resumes.

18.3 A restricted session may send no RPC (`ERR_NOT_PAIRED`), and only data frames whose
`job_id` is the ticket's: a `JobOpen` only of kind `JOB_UPLOAD` without `src` into the ticket's
`root`. Any other frame closes it. A ticket's session replaces (§8) only an earlier session for
the same job from the same key, so one console's sends of different jobs run side by side.

18.4 The session A dialled is held to the same rule from A's side: B may send no RPC, nothing
on the lanes, and on the control connection only a receiver's frames (`JobOpenAck`, `JobMap`,
`Received`, `Credit`, `FileRetry`, `Durable`, `Status`, `JobDone`, `JobCancel`) for the one job;
anything else closes it. No lane may join it, and no handshake to A replaces it.

