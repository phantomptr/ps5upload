# tailnet test bed

A local Headscale, a local `derper` (DERP over TLS with a throwaway CA, plus STUN), one
stock Linux `tailscaled`, and a Linux box (`gcc:14`) that builds and runs our tnctl, in
Docker on this machine (network `tnbed`, 172.31.77.0/24, derper at .10). No Tailscale
account, control server or public DERP is involved.

Pinned images (in `tb.sh`): `headscale/headscale:0.29.1`, `tailscale/tailscale:v1.104.4`,
`gcc:14`. derper is built once with Go from a tailscale clone (`TAILSCALE_SRC`, default
`~/workspace/github.com/tailscale/tailscale`) into `.state/bin/derper`.

## Start and stop

```sh
tailnet/testbed/tb.sh up      # Headscale on http://127.0.0.1:18080, peer "linux-peer" joined
tailnet/testbed/tb.sh status  # the peer's `tailscale status`
tailnet/testbed/tb.sh nodes   # `headscale nodes list`
tailnet/testbed/tb.sh down    # stop and remove the containers (state is kept)
tailnet/testbed/tb.sh build-linux   # tnctl (ASan+UBSan) + tnecho, built in the tn box
tailnet/testbed/tb.sh block on|off  # drop UDP between the peer and the tn box (forces DERP)
tailnet/testbed/tb.sh reset   # down, then move .state/ aside for a clean bed
```

`TNBED_PORT` changes the published Headscale port. Everything the bed writes lives in
`tailnet/testbed/.state/` (git-ignored): Headscale's database and keys, the peer's
state, run logs, and an empty Docker config.

Docker's credential helper can hang on macOS, so `tb.sh` and `m1-check.sh` set
`DOCKER_CONFIG` to `.state/docker` with an empty `config.json`.

## Pre-auth keys

```sh
KEY=$(tailnet/testbed/tb.sh authkey)    # reusable, 24 h, user "tnbed"
tailnet/build/host/tnctl login --control http://127.0.0.1:18080 --authkey "$KEY" --state /tmp/tn1
```

`up` creates the user `tnbed` on first use and joins the Linux peer with such a key.

## Interactive login

Without `--authkey`, `tnctl` prints a URL like
`http://127.0.0.1:18080/register/hskey-authreq-…` and waits (the register followup).
Approve it with the id at the end of the URL:

```sh
tailnet/testbed/tb.sh register hskey-authreq-…
```

## Checks

- `m3-check.sh`: our node as the PS5 helper runs it (`--serve tcp:9295,tcp:9120,udp:9296,
  udp:9302`): TCP through lwIP to local echo services (round trips, 50 MiB bulk), UDP
  relay, RST for a closed local port, allow-list drops, 4 parallel connections, the
  Headscale ACL enforced (policy in database mode), the same over DERP, pause/resume.
- `tb.sh test-linux`: the unit tests on glibc (its UBSan catches what macOS libc does not).

- `m2-check.sh`: the M2 acceptance run, tnctl (Linux, ASan+UBSan) against the stock
  peer: ICMP ping and UDP echo direct and over DERP with RTTs, the peer's `tailscale ping`
  and kernel `ping` to us, direct -> DERP -> direct while UDP is blocked mid-session,
  pause/resume, and the key-expiry warning (Headscale `nodes expire -e`).

- `m1-check.sh`: the M1 acceptance run (pre-auth key, pause/resume, interactive URL,
  TLS through `tlsproxy.py` with a throwaway CA). Leaves the bed running.
- `vectors/gen.sh`: regenerates `tailnet/tests/vectors_go.h` from Tailscale's Go code
  (needs Go and a clone of github.com/tailscale/tailscale; `TAILSCALE_SRC` points at it).
