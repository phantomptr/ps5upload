# Lab

Hand tools for poking a live PS5. These are deliberately manual — one
frame, one question, no workflow around it. When something on hardware
behaves oddly and you want to know exactly what the payload answered,
this is where you come.

For automated end-to-end checks use [`../smoke-hardware.mjs`](../README.md)
instead; for the full gate see [`../../TESTING.md`](../../TESTING.md).

## Pointing them at your console

Every script reads `PS5_ADDR` (or takes the address as its first
argument), so nothing here is tied to one network:

```sh
export PS5_ADDR=192.168.1.50:9113      # transfer port
./status-runtime.sh
./hello-runtime.sh
```

The payload's management port is 9114; the console's ELF loader is
typically 9021. The committed defaults are generic — keep your own
addresses in a local env file rather than editing these scripts.

## What's here

**Runtime state** — `hello-runtime.sh`, `status-runtime.sh`,
`check-runtime-port.sh`, `smoke-runtime.sh`

**Transactions** — `begin-tx.sh`, `query-tx.sh`, `abort-tx.sh`,
`exercise-tx-stub.sh`, `full-cycle.sh`

**Payload lifecycle** — `send-payload.sh`, `shutdown-runtime.sh`,
`takeover-runtime.sh`, `verify-takeover.sh`,
`reload-and-verify-takeover.sh`, `reload-and-verify-replay.sh`

**Diagnostics** — `capture-runtime-trace.sh`, `ftx2_control.py`,
`ftx2_probe.py`, `elev_probe`

**Install/launch probes** — `test_install_launch.py`,
`test_launch_only.py`

`ftx2_control.py` is the most useful of these: it sends an arbitrary
frame and prints the raw reply, which is how you tell "the payload said
no" apart from "the engine mangled it".

## Don't delete these because nothing calls them

`scripts:audit` marks lab utilities as intentionally manual. A script
with no caller is the normal state here, not dead code.
