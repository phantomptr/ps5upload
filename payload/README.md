# Payload

The C payload that runs on the PS5. Built with the
[PS5 Payload SDK](https://github.com/ps5-payload-dev/sdk) (version pinned
in [`../scripts/ps5-sdk.env`](../scripts/ps5-sdk.env)), sent to the
console's ELF loader, and left resident until reboot or rest mode.

It listens on one port, **9120**, for AVA1 (`protocol/ava1/SPEC.md`):
transfers, status, filesystem, mount, app, hardware, package and shell
calls all travel over one encrypted session with up to 8 data lanes.

## Layout

`src/main.c` handles startup: credential elevation, runtime ownership,
the takeover, the one-time removal of the retired transfer folders
(`src/state_migrate.c`), starting the AVA1 server, and cleanup. It binds no
socket of its own. `src/runtime.c` holds the management handlers the AVA1
table (`src/mgmt_table.def`, dispatched by `src/mgmt_rpc.c`) calls, and the
instance lifecycle (ownership record, reap, shutdown). `ava1/` is the AVA1 protocol in C (frames,
generated codecs in `ava1/gen/`, Noise handshake, server); never edit
`ava1/gen/` by hand. `src/takeover.c`
asks an older resident payload to stand down before binding.

The rest of `src/` is one module per capability: registration and launch,
package install, hardware and sensors, processes, profiles, saves and
backup, cheats, FTP, remote play, fan curve, notifications, system
registry and time, firmware spoofing, and the SDK version changer.

`installer/` builds our own standalone install daemon
(`ps5upload-installer.elf`, TCP :9115) used for package installs on newer
firmware. It loads as a companion image on the :9021 loader (never evicting
the helper), self-escalates, and answers a JSON-lines protocol
(`hello`/`install`/`job`/`stop`); for a staged file it serves the package to
Sony's installer over a 127.0.0.1 HTTP URL. The engine sends it automatically
when nothing is listening on :9115.

### Prior art

The installer daemon is our own implementation. Its design was informed, as
behavioural references only (no code or names are carried into the daemon),
by cy33hc's ps5-ezremote-dpi, etaHEN and elf-arsenal's DPI payloads, and
itsPLK's on-console PKG Manager (the loopback-serving + cacheability rules).

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
`make test-payload` with the same warning flags.

See `tests/*_selftest.c` for the full list (hardware guards, ptrace recovery, `app.db`
reading, FTP, SDK param rewriting, installer, cheats, wake watchdog and more). AVA1 itself is
tested from Rust: `cargo test -p ava1-ctest -- --test-threads=1` in `engine/` compiles
`ava1/` on the host and checks it against the Rust side.

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
