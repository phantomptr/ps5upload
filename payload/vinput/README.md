# ps5u-vinput: the keyboard & mouse companion

The console's virtual keyboard and mouse (`libSceMouse` / `libSceKeyboard` virtual devices) are
driven from this small process, never from the helper. The helper embeds the stripped ELF
(`build/vinput_elf.c`), sends it to the console's own loader on `127.0.0.1:9021` when a keyboard
& mouse session opens, and relays to it (`src/vinput_relay.c`). The ELF is embedded raw, not gzipped:
the helper has no inflater, and linking zlib into the helper crashed it before.

| | |
|---|---|
| Process / thread name | `ps5u-vinput`. It must not start with `ps5upload`, because the helper's own-instance sweep kills that prefix. |
| Socket | `127.0.0.1:9184` only (`VWIRE_PORT`). A second instance cannot bind, so it exits. |
| Files | `/data/ps5upload/vinput.token` (0600; the helper writes it, the companion reads and deletes it), `vinput.pid`, `vinput.handles` (its open devices), `vinput.log` |
| Protocol | `include/vinput_wire.h`: binary frames, little-endian; HELLO (token) / CAPS / OPEN / REPORT (the 16-byte report of `include/vinput.h`) / HB / CLOSE, each answered with the same seq and an rc |
| Threads | One (`src/vinput_companion.c` is a poll loop) |
| Sources | `main.c`, `../src/vinput_companion.c` (loop, host-tested), `../src/vinput_dev.c` (the Sony calls), `../src/vinput_wire.c` |

## Lifetime

- **Start:** token read and deleted → bind → pid file → `vinput_dev_init` (sceUserServiceInitialize; load libSceMouse, libSceKeyboard, libSceMbus, libScePad; sceMouseInit / sceKeyboardInit; delete any devices a killed predecessor left behind) → listen.
- **Connections:** only one, which must present the token within 2 s.
- **Devices:** made per OPEN for the foreground user, and made again when that user changes.
- **2 s with no frame from the helper:** everything let go, the devices removed.
- **The helper's connection closes** (it stopped, was replaced, or died): devices removed, the companion exits.
- **Exits after:** 30 s with no device open, or 10 s with nobody presenting the token.

## Helper side (`src/vinput_relay.c`)

- **Start:** a session open while the companion is not running starts it on a relay thread and answers `vinput_starting`. The engine asks again every 300 ms.
- **Timeouts:** each request waits at most 200 ms. A companion silent for 1 s is SIGKILLed by pid.
- **Restarts:** after a SIGKILL it is started again with backoff (1, 2, 4 s). After 3 failures in a row it is `unavailable` (`vinput_companion_failed`), retried after 30 s. The devices the session had are made again.
- **No loader on :9021:** `unavailable` (`vinput_needs_loader`), asked again after 10 s.
- **Status for the app:** caps carries `companion` (idle / starting / running / stopped / unavailable).
- **Liveness:** the relay sends a heartbeat every 300 ms.

## Measured on the fat PS5 (FW 13.60, 2026-10-31; PS5Term's monitor build reading the mouse and keyboard)

- **Companion alone** (a scratch driver on the console): PS5Term saw:
  - all 40 square moves, left and right click (buttons 0x1 / 0x2) and wheel ±1 ×3;
  - `h e l l o` as HID usages 11, 8, 15, 15, 18;
  - each event about 4–15 ms after its insert.
- **Through the helper** (engine `/api/ps5/vinput/ws`): the same, all of it.
  - Cold start, session open to devices up: 0.68 s.
- **Companion SIGKILLed mid-session:**
  - The helper kept answering status and file reads (≤ 10 ms).
  - Caps went running → stopped → running.
  - Motion resumed after a 1.8 s gap.
- **The page going quiet** (socket open): let go after 2.0 s.
- **The engine killed:** the helper's watchdog let go after 2.2 s.
- **The stream socket closing:** let go after 0.5 s.
- **The helper replaced mid-session:** the old companion removed its devices and exited. The new helper's companion took over, and motion resumed after about 4 s.

### Devices left behind by a killed process

- A process ended by SIGKILL never runs DeleteDevice. Its virtual mouse and keyboard stay, holding the user's slot 0.
- Every later device then lands on slot 1: the handle's low bits are 1, e.g. 0x3D0001 where 0x3B0000 came before. A reader of slot 0 (PS5Term's `sceMouseOpen(user, 0, 0)`) sees nothing, even after reopening its handle.
- Calling DeleteDevice with the leaked handles **from a new process** works (rc 0), and new devices are back on slot 0.
- So the companion keeps its open handles in `vinput.handles`, and the next one deletes them first. This was verified on hardware, so no console restart is needed.

## Why not in the helper: the Init hang (for the later merge-back)

Calling `sceMouseInit()` / `sceKeyboardInit()` in the helper's process (commit 65ad3683, on an AVA1 handler thread under `sony_api_lock`) made the whole helper stop answering, with its port still open. That helper (pid 430 on the Phat) is still listed and could not be killed: it is stuck inside the call, uninterruptibly. That is strong evidence the in-process path must not run on the helper's threads as it was.

What Init does, from a disassembly of the 13.60 modules dumped from a working payload:

- **sceMouseInit:**
  1. Returns 0 if it already ran.
  2. Creates and locks a "SceMouseLock" mutex.
  3. Calls **`sceMbusInit`** (an import from libSceMbus).
  4. Opens `/dev/hid`.
  5. Zeroes its tables, makes three libkernel calls and unlocks.
  - No thread creation, no sceUserService.
- **sceKeyboardInit:** the same, minus the three libkernel calls.
- **sceMbusInit:**
  - Its state word is swapped atomically from 0 to 1. A second caller spins on `usleep(10)` for as long as the state is 1, with no timeout.
  - It builds an IPC (Ipmi) client and calls its connect method up to 50 times, sleeping 100 ms between failures.
  - **Nothing bounds a connect that never returns.**

Ranked hypotheses, all unproven:

1. **libSceMbus was not loaded in the helper.** The helper loaded only libSceMouse and libSceKeyboard. The working payload (and this companion) also loads libSceMbus and libScePad. Loading Mouse does not pull Mbus in: it got its own handle and was mapped after Keyboard. So in the helper, Init's lazily bound `sceMbusInit` slot would go through the libkernel resolver for a symbol that is not there. If that blocks inside the dynamic linker while holding its lock, or stops the process for a core dump, the whole helper freezes with its port open. It would also be unkillable, which matches pid 430.
2. **The Mbus IPC connect blocks in the kernel.** The helper's credentials differ: uid 0, every capability, the ptrace bit, authid 0x4800000000000006 against the payload's 0x480000001000000e. Other helper threads also swap the authid process-wide while this runs. The call held `sony_api_lock`, vinput's lock and a `pthread_once`, so every Sony frame queued behind it. That alone would not stop non-Sony frames, though.
3. **A stuck Mbus state word** (the spin above) after a first, stalled Init.
4. **`open("/dev/hid")` blocking:** unlikely.

To merge back: load libSceMbus (and libScePad) explicitly first, and call Init outside `sony_api_lock` and outside any `pthread_once` other handlers wait on. Run it on a thread of its own, as `vinput_dev.c` is written to allow: no locks, one caller. First try it in a **disposable payload** with the helper's credentials and authid swaps. Never try it on the helper. Experiments, cheapest first:

- Read the `sceMbusInit` GOT slot with and without libSceMbus loaded.
- Call Init with Mbus missing, on a thread, while the main thread writes a heartbeat file.
- Call `sceMbusInit` under the helper's ucred while another thread swaps the authid.

`AddDevice`'s `out`: the library only passes it to `ioctl(/dev/hid, 0xC018482A)`. Only bytes 0–3 changed in a zeroed 256-byte buffer, so it is a 4-byte handle. `vinput_dev.c` still passes 256 bytes, as the measured payload did.
