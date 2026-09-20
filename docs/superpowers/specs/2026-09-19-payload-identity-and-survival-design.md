# Payload identity and survival under third-party loaders

Users who load `ps5upload.elf` through ps5-payload-manager's autoloader report
that the helper "goes down" seconds to a minute after launch, comes back when
relaunched from the console, and dies again. Loading the same ELF from our own
client works. This spec makes the payload identifiable, independent of whoever
launched it, and able to say how its predecessor died — and fixes one confirmed
bug found while investigating.

Research for this spec: the source of
[ps5-payload-elfldr](https://github.com/ps5-payload-dev/elfldr) at `02cfe91`
(tags v0.21–v0.26), itsPLK's `ps5-payload-manager` v0.5.1 and `ps5-pkg-manager`,
our own payload and client, and the kernel log attached to issue #289.

---

## What the investigation established

### F1 — elfldr does not share processes between payloads

`elfldr_spawn` (`elfldr.c:603`) creates a fresh process per payload via
`rfork_thread` + ptrace + exec. The only process elfldr ever kills by name is
another `elfldr.elf` (`socksrv.c:414`). Its README states payloads survive each
other's crashes. **The "pldmgr and ps5upload share a host process" theory is
refuted.**

### F2 — a payload's stdin, stdout and stderr are the launcher's socket, for life

`elfldr.c:471-483` dup2s the sender's TCP socket onto all three descriptors.
pldmgr closes its end immediately after streaming the ELF
(`ps5_launcher.c:78`). We redirect stderr to a file (`payload/src/main.c:356`)
but never stdout or stdin, so we keep a live dependency on a socket whose peer
may be long gone.

### F3 — every raw-streamed payload is named `payload.elf`

elfldr takes the process name from the URI filename (`elfldr.c:704` →
`uri_get_filename`). A raw byte stream carries no URI, so `uri_get_filename("")`
returns 0 and the name falls back to the literal `"payload.elf"`
(`socksrv.c:231`, `socksrv.c:280`). pldmgr streams raw bytes; so does our client.

### F4 — the scene's "replace my predecessor" idiom kills by thread name

```c
syscall(SYS_thr_set_name, -1, "<name>.elf");
while ((pid = find_pid("<name>.elf")) > 0) kill(pid, SIGKILL);
```

Verbatim in pldmgr (`src/main.c:86`), pkgmgr (`src/main.c:127`) and elfldr
(`socksrv.c:414`). All three read `ki_tdname` at kinfo offset 447 — the same
field and offset our own `proc_name_by_pid` uses (`payload/src/proc_list.c:356`).
Any payload that runs this idiom against `payload.elf` SIGKILLs every
raw-loaded payload on the console.

### F5 — our observable process name is a worker thread's name

We rename workers (`wake_watchdog.c:126`, `hw_info.c:1030`, `smp_meta.c:302`)
but never the main thread. The `SceShellCore` FMEM dump in issue #289 lists our
process as **`ps5upload-wake`**, alongside `elfldr.elf`, `pldmgr.elf` and
`onion_*.elf`. So the kernel's representative thread for a `KERN_PROC_PROC`
record is not reliably the main thread, and our name is effectively
nondeterministic.

### F6 — elfldr v0.23 → v0.26 changes nothing relevant

Five commits: HTTP GET support, a `uri=` parameter, a `pipe=` parameter, two
cosmetics. Advising users to update elfldr cannot fix a payload that dies. This
advice is currently being given in the project's Discord and should stop.

### F7 — the client's redeploy loop is not the cause

`AppShell.tsx:618-745` takes a fresh probe before every send, holds when the
console answers, holds when the engine is unreachable, holds during a transfer,
and gates to 30 s per host. `ensurePayloadCurrent` skips the send when the
running version matches. An earlier hypothesis that this loop caused the flap
was wrong.

### F8 — CONFIRMED BUG: we refuse to reap our own wedged predecessor

`runtime_reap_prior_instance` gates its SIGKILL on
`strcmp(my_name, their_name) != 0` (`runtime.c:2062`). It is called at
`main.c:483` and `main.c:500` — **before** `start_wake_watchdog()` at
`main.c:552`. So:

* our own name is read before any worker starts → `payload.elf`;
* the prior instance has been running long enough to have started its workers
  → can read as `ps5upload-wake` (F5).

The names differ, we log `reap: pid N is 'ps5upload-wake', not our
'payload.elf' — recycled pid, skipping`, the wedged predecessor survives, the
second `runtime_try_takeover` fails, and the new payload exits at `main.c:485`.
The user sees "I sent the payload and nothing changed" and a helper that never
comes back. This is live in the current build and is independent of which
loader was used.

### What remains unproven

No log yet captures the kill itself. F3 + F4 + F5 together are a sufficient
mechanism for the reported symptom, and they explain why it is launcher-
dependent (our client sends one payload; pldmgr's autoload sends several
500 ms apart, `autoload.c:190`) and why it leaves no crash trace (SIGKILL is
uncatchable). They do not explain ppeterr's "half a minute". Sections 3 and 4
below exist to settle this from the next user report rather than the one after.

---

## Design

### 1. Deterministic identity

Set the main thread's name as the **first statement** in `main()`, before
`umask(0)` and before `runtime_apply_ucred_jailbreak()`:

```c
(void)syscall(SYS_thr_set_name, -1, "ps5upload.elf");
```

This must precede everything else so no other payload can observe us under the
generic name, and so it is set before we spawn any worker.

Because the representative thread is not deterministically main (F5), naming
main alone is not sufficient. Every thread we create already shares a
`ps5upload` prefix; the main thread now joins them. Identity is therefore
tested by prefix, via a new pure helper in `proc_list.c`:

```c
/* True when `name` is one of OUR threads: "ps5upload.elf", "ps5upload-wake",
 * "ps5upload-fan", "ps5upload-smp". Matching on the shared prefix rather than
 * an exact string is required because the kinfo_proc representative thread is
 * not deterministically the main thread (see #289's kernel log). */
int proc_name_is_ours(const char *name);
```

`runtime_reap_prior_instance`'s exact `strcmp` (`runtime.c:2062`) is replaced by
`proc_name_is_ours(their_name)`. This fixes F8.

**Safety.** The name test is the *second* line of defence against pid recycling;
the primary gate is the boot-session check at `runtime.c:2035`, which is
unchanged. Widening from an exact match to a `ps5upload` prefix does not weaken
it meaningfully — no other homebrew carries that prefix — and the existing
requirements still hold: the pid must be one we recorded ourselves, be alive,
not be us, and come from the current boot.

**Explicitly out of scope:** we never sweep on the *generic* name. A
`find_pid("payload.elf")` sweep would hit kstuff, SMP, nanoDNS and every other
raw-loaded payload — `runtime.c:1998` already documents why, and that reasoning
stands. The §5 sweep is a different thing: it matches only the `ps5upload`
prefix, which no other homebrew uses, so it can only ever find our own
instances.

**Known transition gap.** A predecessor from a build *before* this change may
still report as `payload.elf`, which `proc_name_is_ours` will not match — so on
the single upgrade cycle it cannot be reaped by §1 or swept by §5. We
deliberately do not special-case `payload.elf`: accepting it would reopen
exactly the bystander risk `runtime.c:1998` warns about. The graceful
`TAKEOVER_REQUEST` path still handles the healthy predecessor, and the gap
closes permanently once the new build is the one running.

### 2. Stdio independence

Rename `redirect_stderr_to_file()` (`main.c:356`) to `redirect_stdio_to_file()`
and extend it to:

* `dup2` the log fd onto `STDOUT_FILENO` as well as `STDERR_FILENO`;
* open `/dev/null` `O_RDONLY` onto `STDIN_FILENO`.

Call site is unchanged (`main.c:437`, immediately after
`runtime_ensure_directories()`). All 23 `printf()` sites fire after this point —
`runtime_try_takeover` runs at `main.c:471` — so no startup output is lost; it
moves from the launcher's socket into `stderr.log`, where the bug bundle
already collects it.

After this the payload holds no reference to the launcher's socket, and
pldmgr's immediate `close()` cannot affect us regardless of what elfldr wired up.

### 3. How the previous instance ended

The ownership record already serves as an exit marker: written by
`runtime_write_ownership` at startup, unlinked by `runtime_clear_ownership` on
graceful exit (`main.c:654`). What is missing is reading it as a verdict.

A new pure classifier, unit-testable without a console:

| record present | from this boot | prior pid alive | verdict |
|---|---|---|---|
| no | — | — | `clean` (or first run) |
| yes | yes | no | **`killed-externally`** |
| yes | yes | yes | `wedged` |
| yes | no | — | `stale` |

`killed-externally` is the SIGKILL/OOM signature and is otherwise invisible,
because SIGKILL cannot be caught or logged by its victim. The verdict is:

* written to `startup.log` and `stderr.log` as one line;
* stored on `runtime_state_t` and returned in the mgmt-port status response, so
  the client can surface it and the bug bundle captures it.

The inputs (`runtime_read_prior_pid`, `runtime_read_prior_started_at`,
`runtime_system_boottime_unix`) all exist; only the classification and
reporting are new.

### 4. Neighbour census

One `sysctl(KERN_PROC_PROC)` walk at startup, logging to `stderr.log` every
process that looks like homebrew — pldmgr's heuristic, which we can reuse:
`app_id == 0` and the name ends in `.elf`. Cost is one sysctl, once.

This is the data absent from every report in this investigation. With it, a
single future bug bundle answers "what else was on the console, and was
anything else named `payload.elf`" — which confirms or kills the F3/F4
mechanism outright.

### 5. Takeover fallback instead of giving up

Today the second `runtime_try_takeover` failure exits the process
(`main.c:485-492`), leaving the user with a wedged old payload and no new one.

Replace that dead end with a last-resort sweep, reached **only** after the
cooperative handshake has already failed twice:

1. Walk `KERN_PROC_PROC` for processes where `proc_name_is_ours(name)`,
   excluding our own pid.
2. Apply the same boot-session guard used by `runtime_reap_prior_instance`.
3. `SIGKILL`, then poll ~1 s for death (same pattern as `runtime.c:2073-2081`).
4. Retry `runtime_try_takeover` once. If it still fails, exit as today.

The graceful `TAKEOVER_REQUEST` path stays primary because it calls
`runtime_mark_active_transactions(..., "interrupted")` before setting
`shutdown_requested` (`runtime.c:16074`), which tears the journal down cleanly
so upload resume survives. A SIGKILL skips that, so it must never be the
first choice.

This is the belt to §1's braces: §1 fixes the common case where the ownership
record identifies the predecessor; §5 covers the case where that record was
lost or overwritten.

### 6. Surfacing

* Connection screen shows the previous-instance verdict when it is not `clean`.
* The diagnostic bundle includes the verdict and the neighbour census.
* A log line when a neighbour named `payload.elf` is present, since that is the
  condition under which F4 can reach us.

---

## Testing

**Unit (host, no console).** `proc_name_is_ours` and the ownership-verdict
classifier are pure functions and get self-tests under `payload/tests/`,
following the existing `*_selftest.c` pattern.

**Hardware.** Both consoles (Pro, FW 9.60; Phat, FW 5.10):

1. Load via pldmgr's autoloader. Confirm the process appears as
   `ps5upload.elf` in pldmgr's own process list — this also verifies F5's
   representative-thread behaviour is now pinned.
2. `SIGKILL` the payload externally, relaunch, confirm the startup line reports
   `killed-externally`.
3. Wedge a predecessor (suspend it), send a fresh ELF, confirm §5 recovers
   instead of exiting.
4. Confirm `stderr.log` now contains the startup `printf` output that
   previously went to the launcher's socket (§2), and the neighbour census.

**Regression.** A normal client send with a healthy running payload must still
take the graceful `TAKEOVER_REQUEST` path — §5 must not fire. Verify from
`stderr.log` that no SIGKILL sweep is logged.

---

## Follow-up, not part of this spec

A PR to ps5-payload-manager changing `ps5_launch_elf` (`src/ps5_launcher.c:19`)
to use elfldr's URI form with the real filename and `pipe=0` —
`file:/data/pldmgr/payloads/<name>.elf?pipe=0` — falling back to the raw stream
on elfldr < 0.25. That names every payload correctly and gives none of them a
stdio socket, fixing F2 and F3 for the whole ecosystem rather than just for us.
Worth offering; nothing here depends on it.
