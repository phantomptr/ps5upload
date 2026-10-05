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
export PS5_ADDR=192.168.1.50           # console address
./status-runtime.sh
./hello-runtime.sh
```

The helper's only port is 9120 (AVA1); the console's ELF loader is
typically 9021. The committed defaults are generic — keep your own
addresses in a local env file rather than editing these scripts.

## What's here

**Runtime state** — `hello-runtime.sh`, `status-runtime.sh`,
`check-runtime-port.sh`, `smoke-runtime.sh` (the first two call
`ps5upload-lab hello` / `status`)

**Payload lifecycle** — `send-payload.sh`, `shutdown-runtime.sh`,
`reload-and-verify-takeover.sh`

**Diagnostics** — `capture-runtime-trace.sh`, `elev_probe`

For anything else, use `ps5upload-lab` (see `engine/crates/ps5upload-lab`):
every command goes over AVA1, takes the console's host, and accepts a
trailing `:port` that it ignores.

## Don't delete these because nothing calls them

`scripts:audit` marks lab utilities as intentionally manual. A script
with no caller is the normal state here, not dead code.
