# Payload

The C payload that runs on the PS5. Built with the
[PS5 Payload SDK](https://github.com/ps5-payload-dev/sdk) (version pinned
in [`../scripts/ps5-sdk.env`](../scripts/ps5-sdk.env)), sent to the
console's ELF loader, and left resident until reboot or rest mode.

It listens on two ports:

| Port | Accepts |
|---|---|
| **9113** | Transfer frames only |
| **9114** | Everything else — status, filesystem, mount, app, hardware, package, shell |

A frame sent to the wrong port is answered with `wrong_port` rather than
half-working.

## Layout

`src/main.c` handles startup: credential elevation, runtime ownership,
the management thread, the transfer loop, and cleanup. `src/runtime.c` is
the FTX2 runtime itself — framing, the transaction journal, resume,
direct and spooled writes, and most command handlers. `src/takeover.c`
asks an older resident payload to stand down before binding.

The rest of `src/` is one module per capability: registration and launch,
package install, hardware and sensors, processes, profiles, saves and
backup, cheats, FTP, remote play, fan curve, notifications, system
registry and time, firmware spoofing, and the SDK version changer.

`dpi/` builds a separate standalone install daemon
(`ezremote-dpi.elf`, port 9040) used for package installs on newer
firmware. The engine sends it automatically when nothing is listening
there.

## Build

```sh
export PS5_PAYLOAD_SDK=/opt/ps5-payload-sdk
make -C .. payload        # → payload/ps5upload.elf (+ .gz)
make -C .. send-payload PS5_HOST=<ip>
```

Everything compiles with `-Wall -Wextra -Werror`.

## Testing

Logic that can be separated from the console lives in a header under
`include/` and gets a host-compiled self-test in `tests/`, run by
`make test-payload` with the same warning flags. Current cores:

| Header | Self-test covers |
|---|---|
| `hw_guard.h` | Recovering from a faulting Sony getter without losing the helper |
| `ptrace_recovery.h` | Timeout recovery never resuming injected registers |
| `appdb_scan.h` | Reading `app.db` — a real SQLite record reader, because column values are stored with no separators between them |
| `ftp_format.h` | PASV/EPSV/LIST reply shapes, which clients parse strictly |
| `sdk_param.h` | Rewriting `param.json` version fields, and reporting when nothing changed |
| `elf_param.h` | Finding SDK fields via program headers, not by scanning for magic bytes |
| `timed_init.h` | Bounded one-time init that never starts a second initializer |

Prefer adding to this set over testing on hardware: a host self-test runs
in milliseconds and can't wedge a console.

## Things that will bite you

- **Never `rename()` across mounts.** It panics the kernel. Guard on
  `st_dev`.
- **Nothing large on a thread stack.** Threads that need room ask for it
  explicitly (512 KiB–1 MiB); the default is small. A 256 KiB buffer in
  an FTP handler once overflowed it and wedged consoles hard enough to
  need a power cycle — bulk buffers are heap-allocated now.
- **Every blocking wait needs a bound.** Unbounded `ptrace` waits froze
  consoles; Sony IPC init could hang the DPI daemon. Both are now
  timed, and a timed-out attempt is never restarted concurrently.
- **JSON keys must be `snake_case`,** and any interpolated string must be
  escaped. The engine parses with serde: a wrong key silently yields a
  default value, and one bad byte rejects the entire response.
- **Some Sony libraries simply aren't on the console.** `libSceSqlite.sprx`
  is absent under every lib path, so `dlsym(RTLD_DEFAULT, "sqlite3_*")`
  fails on *every* firmware — not just some. Degrade honestly instead of
  blaming the firmware.
- **"The platform doesn't ship it" is not "we can't have it."** That
  SQLite conclusion was right about Sony and wrong about us: a static
  library is just more of our own `.text`. The payload now links its own
  SQLite (`content_db.c`, built from the amalgamation that
  `scripts/install-ps5-sdk.sh` fetches). Two full SQL implementations had
  been sitting behind that dead `dlsym` probe long enough to drift apart
  and hardcode different table names, and a third — in `register.c` — was
  a table of function pointers that nothing ever assigned. A branch that
  can never execute is where bugs go to hide, so prefer deleting it to
  keeping it as a fallback that never fires.
- **Check what a database says its schema is.** `content_db.c` finds the
  app table through `sqlite_master` and `PRAGMA table_info` rather than
  naming it, because the two dead implementations disagreed about the
  name and there was no way to tell which was right.
