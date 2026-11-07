# tailnet test bed

A local Headscale and one stock Linux `tailscaled`, in Docker on this machine. No
Tailscale account and no Tailscale control server are involved. (Headscale fetches
Tailscale's public DERP map, so the bed needs internet access for that one file.)

Pinned images (in `tb.sh`): `headscale/headscale:0.29.1`, `tailscale/tailscale:v1.104.4`.

## Start and stop

```sh
tailnet/testbed/tb.sh up      # Headscale on http://127.0.0.1:18080, peer "linux-peer" joined
tailnet/testbed/tb.sh status  # the peer's `tailscale status`
tailnet/testbed/tb.sh nodes   # `headscale nodes list`
tailnet/testbed/tb.sh down    # stop and remove both containers (state is kept)
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

- `m1-check.sh`: the M1 acceptance run (pre-auth key, pause/resume, interactive URL,
  TLS through `tlsproxy.py` with a throwaway CA). Leaves the bed running.
- `vectors/gen.sh`: regenerates `tailnet/tests/vectors_go.h` from Tailscale's Go code
  (needs Go and a clone of github.com/tailscale/tailscale; `TAILSCALE_SRC` points at it).
