# ps5u-mbusprobe: does binding a pad to a user work on this console?

A throwaway payload, sent on its own. It tries the one Sony call Play Together's "Guests play as"
setting depends on, `sceMbusBindDeviceWithUserId`, outside the helper. If it hangs or crashes, only
this process is affected. It is never linked into or embedded in the helper.

The approach comes from ProsperoSunshine PR #33 (`src/platform/ps5/profile_seat.c`, measured on
FW 10.20). See the header of `main.c`.

## Build

```sh
cd payload/tools/mbus-probe
PS5_PAYLOAD_SDK=/opt/ps5-payload-sdk LLVM_CONFIG=/opt/homebrew/opt/llvm/bin/llvm-config make
```

This produces `ps5u-mbusprobe.elf`.

## Send

Send it the way every payload is sent: raw to the console's ELF loader on port 9021.

```sh
make send PS5_HOST=<console ip>        # = nc -w 1 <console ip> 9021 < ps5u-mbusprobe.elf
```

The PS5Upload app's payload sender (Payloads › Send a file) also works.

The probe reads `/data/ps5upload/mbus-probe.conf` before it does anything. Upload that file first,
using the app's file manager or `files_write`. The file holds `key=value` lines:

```
mode=init          # init (default) or bind
user=0x1a2b3c4d    # bind only: a user id that is SIGNED IN (the log lists them)
hold_ms=5000       # bind only: how long the pad stays, 0-60000
```

With no config file it runs `init`.

## Run 1: `mode=init` (no pad, the safest)

The probe:

- loads `libSceMbus`;
- resolves `sceMbusInit`, `sceMbusBindDeviceWithUserId`, `sceMbusGetDeviceInfo_` and
  `sceMbusDisconnectDevice` by NID;
- calls `sceMbusInit(0, 0)` on a thread of its own, while the main thread rewrites
  `/data/ps5upload/mbus-probe.heartbeat` every 100 ms;
- logs the signed-in users with their ids, then exits.

Read `/data/ps5upload/mbus-probe.log`. A good run looks like this:

```
sceKernelLoadStartModule(libSceMbus.sprx) = 0x…   (or "already loaded")
resolve sceMbusInit (wRPXMGtkOq0): 0x8…           (all four non-zero)
sceMbusInit(0,0) = 0x00000000 after … us
  signed in: 0x… "Name" …                          (note the id of a profile that is NOT yours)
==== done: init only, …
```

What could go wrong:

| What you see | What it means |
|---|---|
| `resolve …: 0x0` | This firmware lacks the function. Stop here and send me the log. |
| `sceMbusInit HUNG` | Init hung, as it did in the helper. The process may stay listed, stuck. Stop and send me the log. Do not run `bind`. |
| The heartbeat file's time stops changing, its stage says `sceMbusInit`, and no `HUNG` line appears | The whole process froze. Send me the log. |
| The console itself misbehaves | Unexpected. Note what happened. |

## Run 2: `mode=bind`

Before this run, sign a second profile in on the PS5 (for example "Guest 1"). Put its id from run
1's log in `user=`. Keep the PS5's home screen in view.

The probe:

- does everything in run 1;
- adds one virtual DualSense the way the helper adds a guest's pad;
- binds it to `user` at once, retrying every 1 ms up to 30 times while it answers `0x809b0005`;
- reads the owner back (`sceMbusGetDeviceInfo_`, `+0xC8`, with a hexdump of the whole block);
- holds the pad `hold_ms`, reads the owner again, then removes the pad.

A good run looks like this:

```
pad add ioctl = 0 …, handle N
sceMbusBindDeviceWithUserId(N, 0x…) = 0x00000000 after T tries, U us after the add
sceMbusGetDeviceInfo_(N) right after = 0x00000000, owner @0xC8 = 0x<the same user>
sceMbusGetDeviceInfo_(N) after the hold = 0x00000000, owner @0xC8 = 0x<the same user>
pad delete ioctl = 0
==== done: bind OK
```

On the TV, check three things:

- **No "Who's using this controller?" question** comes up while the pad is there.
- **The bound profile shows a controller** in the quick menu or on the profile icons.
- **Your own DualSense** still belongs to you.

Please send me:

- the whole log;
- the `tries` count and `us after the add` (the reference measured under 10 ms);
- whether the question appeared;
- the owner value (a different owner, or 0, means the bind did not stick).

Also worth one run: `user=` set to a profile that is **not** signed in. I expect an error code there,
not a sign-in. Report the code. The app's first version never does this.

## Clean up

The probe exits on its own and removes its pad. Delete `mbus-probe.conf`, `mbus-probe.log` and
`mbus-probe.heartbeat` from `/data/ps5upload` when done.
