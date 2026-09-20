# Payload Identity and Survival Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make the PS5 payload deterministically identifiable, independent of whoever launched it, and able to report how its predecessor died — fixing a confirmed bug that prevents it from reaping its own wedged predecessor.

**Architecture:** Five payload-side changes (main-thread naming, stdio detachment, prior-instance verdict, neighbour census, takeover SIGKILL fallback) plus client-side surfacing. All decision logic goes into `static inline` pure functions in headers so it is host-testable without a PS5, following the existing `wake_watchdog_should_recover` pattern.

**Tech Stack:** C (PS5 payload SDK, FreeBSD syscalls), TypeScript/React (client), GNU make.

**Spec:** `docs/superpowers/specs/2026-09-19-payload-identity-and-survival-design.md`

## Global Constraints

- **Branch:** all work lands on `payload-identity-survival`, which already exists and holds the spec commit.
- **Payload JSON keys must be `snake_case`.** The engine deserializes STATUS_ACK with serde; a camelCase key is silently dropped and the field reads as zero/absent. No exceptions.
- **Never reformat the i18n locale files.** They are generated at column 0; running Prettier over them breaks the duplicate-key gate.
- **`client/src/i18n/locales/en.ts` is the source of truth.** Every new key must either be translated in every locale or added to that locale's `missing` array in `scripts/i18n-known-missing.json`, or the i18n coverage gate fails CI.
- **No `fork()` and no cross-mount `rename()`** anywhere in payload code.
- **Payload selftests are host builds:** `cc -O2 -Wall -Wextra -Werror`, so every new header must compile clean under those flags on macOS/Linux with no PS5 SDK headers.
- **Process name prefix is exactly `ps5upload`** — `ps5upload.elf` (main), `ps5upload-wake`, `ps5upload-fan`, `ps5upload-smp` (workers).
- **kinfo_proc offsets:** `KINFO_PID_OFFSET 72`, `KINFO_TDNAME_OFFSET 447` (already defined in `payload/src/proc_list.c:44-45`).

---

### Task 1: `proc_name_is_ours` — the shared-prefix identity test

**Files:**
- Create: `payload/include/proc_identity.h`
- Create: `payload/tests/proc_identity_selftest.c`
- Modify: `Makefile` (the `test-payload` target, after the wake-watchdog selftest block at line ~736)

**Interfaces:**
- Consumes: nothing.
- Produces: `int proc_name_is_ours(const char *name)` — returns 1 when `name` is one of our threads, 0 otherwise. Safe on `NULL` (returns 0). Task 2, Task 6 and Task 7 all use it.

- [ ] **Step 1: Write the failing test**

Create `payload/tests/proc_identity_selftest.c`:

```c
/* Our process name is whichever thread the kernel picks as the kinfo_proc
 * representative — issue #289's kernel log showed the PS5 calling us
 * "ps5upload-wake", not the main thread's name. So identity is a prefix
 * test, never an exact string compare. */
#include "proc_identity.h"

#include <stdio.h>

static int failures = 0;

static void check(int ok, const char *label) {
    printf("  %s %s\n", ok ? "PASS" : "FAIL", label);
    if (!ok) failures++;
}

int main(void) {
    /* Every thread we create. */
    check(proc_name_is_ours("ps5upload.elf"), "main thread");
    check(proc_name_is_ours("ps5upload-wake"), "wake watchdog thread");
    check(proc_name_is_ours("ps5upload-fan"), "fan thread");
    check(proc_name_is_ours("ps5upload-smp"), "smp thread");

    /* The generic name elfldr gives every raw-streamed payload. Matching it
     * would put kstuff, nanoDNS and every other payload in SIGKILL range. */
    check(!proc_name_is_ours("payload.elf"), "generic loader name is NOT ours");

    /* Bystanders. */
    check(!proc_name_is_ours("pldmgr.elf"), "payload manager is not ours");
    check(!proc_name_is_ours("elfldr.elf"), "elfldr is not ours");
    check(!proc_name_is_ours("SceShellUI"), "system process is not ours");
    check(!proc_name_is_ours("shadowmountplus.elf"), "SMP is not ours");

    /* Defensive. */
    check(!proc_name_is_ours(NULL), "NULL is not ours");
    check(!proc_name_is_ours(""), "empty string is not ours");
    check(!proc_name_is_ours("ps5uploa"), "truncated prefix is not ours");

    printf("\nproc_identity_selftest: %s\n",
           failures == 0 ? "ALL PASS" : "FAILED");
    return failures == 0 ? 0 : 1;
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run:
```bash
cc -O2 -Wall -Wextra -Werror -Ipayload/include \
   -o /tmp/ps5upload-proc-identity-selftest \
   payload/tests/proc_identity_selftest.c
```
Expected: FAIL — `fatal error: 'proc_identity.h' file not found`.

- [ ] **Step 3: Write the minimal implementation**

Create `payload/include/proc_identity.h`:

```c
#ifndef PS5UPLOAD2_PROC_IDENTITY_H
#define PS5UPLOAD2_PROC_IDENTITY_H

#include <string.h>

/*
 * Is this process name one of OURS?
 *
 * Every thread the payload creates shares the "ps5upload" prefix:
 * "ps5upload.elf" (main), "ps5upload-wake", "ps5upload-fan", "ps5upload-smp".
 *
 * A prefix test rather than an exact compare is REQUIRED. The kinfo_proc
 * record returned by sysctl(KERN_PROC_*) carries the name of whichever
 * thread the kernel treats as representative, and that is not reliably the
 * main thread: the SceShellCore FMEM dump in issue #289 listed our process
 * as "ps5upload-wake". An exact compare against the main thread's name is
 * exactly the bug that stopped runtime_reap_prior_instance from reaping its
 * own wedged predecessor.
 *
 * It deliberately does NOT match "payload.elf" — the generic name elfldr
 * gives every raw-streamed payload (elfldr.c:704 -> uri_get_filename falls
 * back to the literal when there is no URI). Matching that would put kstuff,
 * ShadowMountPlus, nanoDNS and every other payload on the console inside our
 * SIGKILL radius.
 */
#define PS5UPLOAD2_PROC_PREFIX     "ps5upload"
#define PS5UPLOAD2_PROC_PREFIX_LEN 9

static inline int proc_name_is_ours(const char *name) {
    if (!name) return 0;
    return strncmp(name, PS5UPLOAD2_PROC_PREFIX,
                   PS5UPLOAD2_PROC_PREFIX_LEN) == 0 ? 1 : 0;
}

#endif /* PS5UPLOAD2_PROC_IDENTITY_H */
```

- [ ] **Step 4: Run the test to verify it passes**

Run:
```bash
cc -O2 -Wall -Wextra -Werror -Ipayload/include \
   -o /tmp/ps5upload-proc-identity-selftest \
   payload/tests/proc_identity_selftest.c && /tmp/ps5upload-proc-identity-selftest
```
Expected: PASS — `proc_identity_selftest: ALL PASS`, exit 0.

- [ ] **Step 5: Wire it into the `test-payload` target**

In `Makefile`, immediately after the wake-watchdog selftest block (the line
`@echo "✓ clock adjustments and unknown firmware state cannot trigger kernel writes"`),
insert:

```makefile
	@echo "Running process-identity self-test (host build)..."
	@cc -O2 -Wall -Wextra -Werror -I$(PAYLOAD_DIR)/include \
		-o /tmp/ps5upload-proc-identity-selftest \
		$(PAYLOAD_DIR)/tests/proc_identity_selftest.c
	@/tmp/ps5upload-proc-identity-selftest
	@echo "✓ only our own threads are ever in SIGKILL range"
```

- [ ] **Step 6: Commit**

```bash
git add payload/include/proc_identity.h payload/tests/proc_identity_selftest.c Makefile
git commit -m "feat(payload): shared-prefix process identity test

Our kinfo_proc representative thread is not reliably the main thread (see
issue #289's kernel log, which names us ps5upload-wake), so identity must
be a prefix test. Deliberately excludes the generic payload.elf name that
elfldr gives every raw-streamed payload.

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>"
```

---

### Task 2: Name the main thread, and fix the reap that depends on it

**Files:**
- Modify: `payload/src/main.c` (top of `main()`, line ~374)
- Modify: `payload/src/runtime.c:2052-2070` (the name check in `runtime_reap_prior_instance`)

**Interfaces:**
- Consumes: `proc_name_is_ours(const char *)` from Task 1.
- Produces: the payload's main thread is named `ps5upload.elf`; `runtime_reap_prior_instance` now reaps a predecessor whose representative thread is any `ps5upload*` thread.

- [ ] **Step 1: Name the main thread**

In `payload/src/main.c`, make this the **first statement** in `main()` — before
`g_state = &state;`, before `startup_trace("ENTER_MAIN")`, before `umask(0)`:

```c
int main(void) {
    /* Name ourselves BEFORE anything else can observe us.
     *
     * elfldr names every raw-streamed payload "payload.elf" (elfldr.c:704 ->
     * uri_get_filename falls back to the literal when the ELF arrives as raw
     * bytes rather than a URI). That is the same generic name pldmgr, pkgmgr
     * and elfldr itself sweep for with the scene's standard
     * "kill my predecessor by name" idiom, so wearing it puts us in the blast
     * radius of any payload that runs that idiom.
     *
     * It also breaks our OWN reap: runtime_reap_prior_instance compares our
     * name against the predecessor's, and ours is read before any worker
     * thread starts while the predecessor's is read after — see #289. */
    (void)syscall(SYS_thr_set_name, -1, "ps5upload.elf");

    int rc = 0;
    runtime_state_t state = {0};
    g_state = &state;
    startup_trace("ENTER_MAIN");
    /* ... existing body unchanged ... */
```

Verify `#include <sys/syscall.h>` and `#include <unistd.h>` are already present
at the top of `main.c`; add `#include <sys/syscall.h>` if missing.

- [ ] **Step 2: Replace the exact name compare in the reap**

In `payload/src/runtime.c`, add near the other includes:

```c
#include "proc_identity.h"
```

Then replace lines 2052-2070 (from `char my_name[64] = {0};` through the
closing brace of the `strcmp` block) with:

```c
    char their_name[64] = {0};
    if (proc_name_by_pid(prior, their_name, sizeof(their_name)) != 0) {
        return; /* prior pid vanished between the checks — nothing to do */
    }
    /* Second line of defence against pid recycling. NOT an exact compare
     * against our own name: ours is read here, before any worker thread has
     * started, so it is still "ps5upload.elf", while a predecessor that has
     * been up for a while reports whichever worker the kernel picked as its
     * representative thread — "ps5upload-wake" in issue #289's kernel log.
     * The exact compare made this branch always take the skip path, so a
     * wedged predecessor was never reaped and the new payload exited.
     *
     * The PRIMARY safety gate remains the boot-session check above; this
     * only has to rule out a recycled pid now owned by unrelated homebrew,
     * and no other homebrew carries the "ps5upload" prefix. */
    if (!proc_name_is_ours(their_name)) {
        fprintf(stderr,
                "[payload2] reap: pid %d is '%s', not one of ours — recycled pid, skipping\n",
                prior, their_name);
        return;
    }
```

Note the `my_name` local and its `proc_name_by_pid(me, ...)` call are removed
from this block. The diagnostic at `runtime.c:2016-2021` already logs our own
name on every startup, so nothing is lost.

- [ ] **Step 3: Build the payload**

Run: `make payload`
Expected: builds clean, no warnings about an unused `my_name`.

- [ ] **Step 4: Run the payload test suite**

Run: `make test-payload`
Expected: all selftests pass, including `proc_identity_selftest: ALL PASS`.

- [ ] **Step 5: Commit**

```bash
git add payload/src/main.c payload/src/runtime.c
git commit -m "fix(payload): name the main thread, and reap by prefix not exact match

runtime_reap_prior_instance read our own name before any worker thread
started (payload.elf) and the predecessor's after (ps5upload-wake), so the
strcmp always mismatched, the wedged predecessor was never reaped, the
second takeover failed and the new payload exited. Naming the main thread
ps5upload.elf also takes us out of the payload.elf SIGKILL sweeps that
pldmgr, pkgmgr and elfldr all run.

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>"
```

---

### Task 3: Stdio independence from the launcher's socket

**Files:**
- Modify: `payload/src/main.c:345-372` (`redirect_stderr_to_file`) and its call site at line ~437

**Interfaces:**
- Consumes: nothing.
- Produces: `static void redirect_stdio_to_file(void)` replacing `redirect_stderr_to_file`. After it runs the payload holds no descriptor referencing the launcher's socket.

- [ ] **Step 1: Rewrite the function**

In `payload/src/main.c`, replace `redirect_stderr_to_file` with:

```c
/* Detach the payload's stdio from whoever launched it.
 *
 * elfldr dup2s the SENDER's TCP socket onto stdin, stdout AND stderr
 * (elfldr.c:471-483) and keeps it there for the payload's whole life.
 * ps5-payload-manager closes its end the instant the ELF is streamed
 * (ps5_launcher.c:78), so from then on our stdio points at a socket whose
 * peer is gone. We must not depend on it.
 *
 * stderr already went to a file; stdout did not, and stdin was left as the
 * socket. Now all three are ours: stdout and stderr to the log, stdin to
 * /dev/null. The startup printf() output that used to vanish into the
 * launcher's socket now lands in stderr.log, which the bug bundle collects.
 *
 * Uses dup2 (async-safe, robust): if an open fails the corresponding
 * descriptor is left untouched. Unbuffered so a crash can't lose the tail.
 * One .old generation is kept so it can't grow without bound. */
static void redirect_stdio_to_file(void) {
    const char *path = PS5UPLOAD2_RUNTIME_ROOT "/stderr.log";
    struct stat st;
    if (stat(path, &st) == 0 && st.st_size > 512 * 1024) {
        rename(path, PS5UPLOAD2_RUNTIME_ROOT "/stderr.log.old");
    }

    /* stdin first: a closed socket on fd 0 is a descriptor we do not
     * control, and /dev/null is always safe to read EOF from. */
    int devnull = open("/dev/null", O_RDONLY);
    if (devnull >= 0) {
        dup2(devnull, STDIN_FILENO);
        if (devnull != STDIN_FILENO) close(devnull);
    }

    int fd = open(path, O_WRONLY | O_CREAT | O_APPEND, 0644);
    if (fd < 0) return;
    dup2(fd, STDOUT_FILENO);
    dup2(fd, STDERR_FILENO);
    if (fd != STDOUT_FILENO && fd != STDERR_FILENO) close(fd);
    setvbuf(stdout, NULL, _IONBF, 0);
    setvbuf(stderr, NULL, _IONBF, 0);

    struct timespec ts;
    if (clock_gettime(CLOCK_REALTIME, &ts) != 0) {
        ts.tv_sec = time(NULL);
    }
    fprintf(stderr, "=== ps5upload payload v%s stdio — session start %lld ===\n",
            PS5UPLOAD2_VERSION, (long long)ts.tv_sec);
}
```

- [ ] **Step 2: Update the call site**

At `payload/src/main.c` line ~437, change:

```c
    redirect_stderr_to_file();
    startup_trace("STDERR_REDIRECTED");
```

to:

```c
    redirect_stdio_to_file();
    startup_trace("STDIO_REDIRECTED");
```

Also update the comment two lines above it from "capture stderr to a fetchable
file" to "capture stdout+stderr to a fetchable file and detach stdin".

- [ ] **Step 3: Build**

Run: `make payload`
Expected: clean build. Confirm no other reference to the old name remains:
```bash
grep -rn "redirect_stderr_to_file\|STDERR_REDIRECTED" payload/ client/ engine/
```
Expected: no matches.

- [ ] **Step 4: Commit**

```bash
git add payload/src/main.c
git commit -m "fix(payload): detach stdio from the launcher's socket

elfldr points stdin/stdout/stderr at the sender's TCP socket for the
payload's whole life, and ps5-payload-manager closes its end immediately
after streaming the ELF. stdout and stdin were still pointing there. Now
all three are ours, and the startup printf output lands in stderr.log
where the bug bundle can collect it.

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>"
```

---

### Task 4: The prior-instance verdict classifier

**Files:**
- Create: `payload/include/instance_verdict.h`
- Create: `payload/tests/instance_verdict_selftest.c`
- Modify: `Makefile` (`test-payload` target, after the Task 1 block)

**Interfaces:**
- Consumes: nothing.
- Produces:
  - `typedef enum { PS5UPLOAD2_PRIOR_CLEAN = 0, PS5UPLOAD2_PRIOR_KILLED_EXTERNALLY = 1, PS5UPLOAD2_PRIOR_WEDGED = 2, PS5UPLOAD2_PRIOR_STALE = 3 } ps5upload2_prior_verdict_t;`
  - `ps5upload2_prior_verdict_t instance_verdict_classify(int record_present, uint64_t prior_started_at, uint64_t boottime, int prior_pid_alive)`
  - `const char *instance_verdict_name(ps5upload2_prior_verdict_t v)` → `"clean"`, `"killed_externally"`, `"wedged"`, `"stale"`.

  Task 5 consumes all three.

- [ ] **Step 1: Write the failing test**

Create `payload/tests/instance_verdict_selftest.c`:

```c
/* How the previous payload instance ended. SIGKILL cannot be caught, so the
 * only evidence an externally-killed instance leaves is an ownership record
 * it never got to unlink. */
#include "instance_verdict.h"

#include <stdio.h>
#include <string.h>

static int failures = 0;

static void check(int ok, const char *label) {
    printf("  %s %s\n", ok ? "PASS" : "FAIL", label);
    if (!ok) failures++;
}

int main(void) {
    const uint64_t BOOT = 1000000;
    const uint64_t AFTER_BOOT = BOOT + 500;
    const uint64_t BEFORE_BOOT = BOOT - 500;

    /* No record: the previous instance unlinked it on a graceful exit, or
     * this is the first run ever. */
    check(instance_verdict_classify(0, 0, BOOT, 0) == PS5UPLOAD2_PRIOR_CLEAN,
          "no record means clean exit");
    check(instance_verdict_classify(0, AFTER_BOOT, BOOT, 1) == PS5UPLOAD2_PRIOR_CLEAN,
          "no record wins over every other input");

    /* The signature we care about: a record from THIS boot whose pid is
     * gone. It died without unlinking — SIGKILL or OOM. */
    check(instance_verdict_classify(1, AFTER_BOOT, BOOT, 0)
              == PS5UPLOAD2_PRIOR_KILLED_EXTERNALLY,
          "record from this boot + pid gone = killed externally");

    /* Still running: wedged, and the reap path deals with it. */
    check(instance_verdict_classify(1, AFTER_BOOT, BOOT, 1)
              == PS5UPLOAD2_PRIOR_WEDGED,
          "record from this boot + pid alive = wedged");

    /* The ownership file lives on persistent /data and survives reboots, so
     * a record predating this boot says nothing about this session. */
    check(instance_verdict_classify(1, BEFORE_BOOT, BOOT, 0)
              == PS5UPLOAD2_PRIOR_STALE,
          "record from a previous boot is stale");
    check(instance_verdict_classify(1, BEFORE_BOOT, BOOT, 1)
              == PS5UPLOAD2_PRIOR_STALE,
          "stale wins over pid-alive");

    /* Unknowable inputs must never be reported as a real verdict. */
    check(instance_verdict_classify(1, 0, BOOT, 0) == PS5UPLOAD2_PRIOR_STALE,
          "old-format record with no start time is stale");
    check(instance_verdict_classify(1, AFTER_BOOT, 0, 0) == PS5UPLOAD2_PRIOR_STALE,
          "unavailable boottime is stale");

    /* Names are the wire contract — snake_case, stable. */
    check(!strcmp(instance_verdict_name(PS5UPLOAD2_PRIOR_CLEAN), "clean"),
          "clean name");
    check(!strcmp(instance_verdict_name(PS5UPLOAD2_PRIOR_KILLED_EXTERNALLY),
                  "killed_externally"),
          "killed_externally name");
    check(!strcmp(instance_verdict_name(PS5UPLOAD2_PRIOR_WEDGED), "wedged"),
          "wedged name");
    check(!strcmp(instance_verdict_name(PS5UPLOAD2_PRIOR_STALE), "stale"),
          "stale name");
    check(!strcmp(instance_verdict_name((ps5upload2_prior_verdict_t)99),
                  "unknown"),
          "out-of-range verdict has a name");

    printf("\ninstance_verdict_selftest: %s\n",
           failures == 0 ? "ALL PASS" : "FAILED");
    return failures == 0 ? 0 : 1;
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run:
```bash
cc -O2 -Wall -Wextra -Werror -Ipayload/include \
   -o /tmp/ps5upload-instance-verdict-selftest \
   payload/tests/instance_verdict_selftest.c
```
Expected: FAIL — `fatal error: 'instance_verdict.h' file not found`.

- [ ] **Step 3: Write the minimal implementation**

Create `payload/include/instance_verdict.h`:

```c
#ifndef PS5UPLOAD2_INSTANCE_VERDICT_H
#define PS5UPLOAD2_INSTANCE_VERDICT_H

#include <stdint.h>

/*
 * How did the PREVIOUS payload instance end?
 *
 * The ownership record (runtime_write_ownership / runtime_clear_ownership)
 * is already an exit marker: written at startup, unlinked on a graceful
 * exit. What was missing was reading it as a verdict.
 *
 * This matters because SIGKILL cannot be caught. An instance killed by
 * another payload's "replace my predecessor" sweep, or by the OOM killer,
 * leaves no log line of its own — the ONLY evidence is an ownership record
 * it never got to unlink, plus a pid that is no longer alive.
 */
typedef enum {
    PS5UPLOAD2_PRIOR_CLEAN = 0,
    PS5UPLOAD2_PRIOR_KILLED_EXTERNALLY = 1,
    PS5UPLOAD2_PRIOR_WEDGED = 2,
    PS5UPLOAD2_PRIOR_STALE = 3,
} ps5upload2_prior_verdict_t;

/*
 * `record_present`    — an ownership record existed at startup.
 * `prior_started_at`  — its started_at_unix field, 0 when absent/unreadable.
 * `boottime`          — kern.boottime in the same clock domain, 0 when the
 *                       sysctl is unavailable.
 * `prior_pid_alive`   — kill(pid, 0) succeeded for its recorded pid.
 *
 * Fails safe: any unknowable input yields STALE rather than a confident
 * claim. The ownership file lives on persistent /data and survives reboots,
 * so a record that predates this boot says nothing about this session.
 */
static inline ps5upload2_prior_verdict_t
instance_verdict_classify(int record_present,
                          uint64_t prior_started_at,
                          uint64_t boottime,
                          int prior_pid_alive) {
    if (!record_present) return PS5UPLOAD2_PRIOR_CLEAN;
    if (boottime == 0 || prior_started_at == 0 || prior_started_at < boottime) {
        return PS5UPLOAD2_PRIOR_STALE;
    }
    if (prior_pid_alive) return PS5UPLOAD2_PRIOR_WEDGED;
    return PS5UPLOAD2_PRIOR_KILLED_EXTERNALLY;
}

/* Wire name. snake_case: this value crosses the STATUS_ACK JSON boundary
 * into serde on the engine side. */
static inline const char *
instance_verdict_name(ps5upload2_prior_verdict_t v) {
    switch (v) {
        case PS5UPLOAD2_PRIOR_CLEAN:              return "clean";
        case PS5UPLOAD2_PRIOR_KILLED_EXTERNALLY:  return "killed_externally";
        case PS5UPLOAD2_PRIOR_WEDGED:             return "wedged";
        case PS5UPLOAD2_PRIOR_STALE:              return "stale";
    }
    return "unknown";
}

#endif /* PS5UPLOAD2_INSTANCE_VERDICT_H */
```

- [ ] **Step 4: Run the test to verify it passes**

Run:
```bash
cc -O2 -Wall -Wextra -Werror -Ipayload/include \
   -o /tmp/ps5upload-instance-verdict-selftest \
   payload/tests/instance_verdict_selftest.c && /tmp/ps5upload-instance-verdict-selftest
```
Expected: PASS — `instance_verdict_selftest: ALL PASS`, exit 0.

- [ ] **Step 5: Wire it into the `test-payload` target**

In `Makefile`, immediately after the Task 1 block (`@echo "✓ only our own threads are ever in SIGKILL range"`), insert:

```makefile
	@echo "Running prior-instance verdict self-test (host build)..."
	@cc -O2 -Wall -Wextra -Werror -I$(PAYLOAD_DIR)/include \
		-o /tmp/ps5upload-instance-verdict-selftest \
		$(PAYLOAD_DIR)/tests/instance_verdict_selftest.c
	@/tmp/ps5upload-instance-verdict-selftest
	@echo "✓ an externally-killed predecessor is reported, never guessed at"
```

- [ ] **Step 6: Commit**

```bash
git add payload/include/instance_verdict.h payload/tests/instance_verdict_selftest.c Makefile
git commit -m "feat(payload): classifier for how the previous instance ended

SIGKILL cannot be caught, so an externally-killed payload leaves no log of
its own. The ownership record it never unlinked is the only evidence, and
this turns that into a verdict. Fails safe to 'stale' on any unknowable
input rather than claiming a kill that may not have happened.

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>"
```

---

### Task 5: Compute the verdict at startup and report it on STATUS_ACK

**Files:**
- Modify: `payload/include/runtime.h` (add a field near `startup_reason`, line ~152)
- Modify: `payload/src/runtime.c` (new `runtime_classify_prior_instance`, near `runtime_reap_prior_instance` at line ~2009; `handle_status_frame` at line ~15117)
- Modify: `payload/src/main.c` (call it after `runtime_init`, line ~452)

**Interfaces:**
- Consumes: `instance_verdict_classify`, `instance_verdict_name` (Task 4); the existing file-scope statics `runtime_read_prior_pid`, `runtime_read_prior_started_at`, `runtime_system_boottime_unix` in `runtime.c`.
- Produces:
  - `runtime_state_t.prior_verdict` (an `int` holding a `ps5upload2_prior_verdict_t`)
  - `void runtime_classify_prior_instance(runtime_state_t *state)` — declared in `runtime.h`
  - STATUS_ACK gains `"prior_instance":"<name>"`. Task 8 consumes that key.

- [ ] **Step 1: Add the state field**

In `payload/include/runtime.h`, add `#include "instance_verdict.h"` near the
other includes, and add this field immediately after `int startup_reason;`:

```c
    /* How the PREVIOUS instance ended, classified once at startup before
     * runtime_write_ownership overwrites the record. Holds a
     * ps5upload2_prior_verdict_t. Reported on STATUS_ACK so the client and
     * the bug bundle can show it — an externally SIGKILLed predecessor is
     * otherwise completely invisible. */
    int prior_verdict;
```

Also declare, next to the other `runtime_*` prototypes:

```c
/* Classify how the previous instance ended and store it on `state`.
 * MUST be called after runtime_init (which fills ownership_path) and
 * BEFORE runtime_write_ownership overwrites the prior record. */
void runtime_classify_prior_instance(runtime_state_t *state);
```

- [ ] **Step 2: Implement it**

In `payload/src/runtime.c`, add `#include "instance_verdict.h"` near the other
includes, then insert this function immediately **before**
`runtime_reap_prior_instance`:

```c
void runtime_classify_prior_instance(runtime_state_t *state) {
    if (!state) return;

    struct stat st;
    int record_present = (stat(state->ownership_path, &st) == 0) ? 1 : 0;
    uint64_t prior_started = 0;
    int prior_alive = 0;

    if (record_present) {
        prior_started = runtime_read_prior_started_at(state->ownership_path);
        int prior_pid = runtime_read_prior_pid(state->ownership_path);
        if (prior_pid > 0 && prior_pid != (int)getpid()) {
            prior_alive = (kill((pid_t)prior_pid, 0) == 0) ? 1 : 0;
        }
    }

    ps5upload2_prior_verdict_t v =
        instance_verdict_classify(record_present, prior_started,
                                  runtime_system_boottime_unix(), prior_alive);
    state->prior_verdict = (int)v;

    fprintf(stderr, "[payload2] prior instance: %s\n", instance_verdict_name(v));
    if (v == PS5UPLOAD2_PRIOR_KILLED_EXTERNALLY) {
        fprintf(stderr,
                "[payload2] the previous instance was killed by something else on this "
                "console (SIGKILL or OOM) — it did not exit on its own\n");
    }
}
```

- [ ] **Step 3: Call it from main()**

In `payload/src/main.c`, immediately after `startup_trace("RUNTIME_INIT_DONE");`
(line ~452), insert:

```c
    /* Before the takeover and reap touch the ownership record, and long
     * before runtime_write_ownership overwrites it, read it as evidence of
     * how the last instance ended. */
    runtime_classify_prior_instance(&state);
    startup_trace("PRIOR_VERDICT_DONE");
```

- [ ] **Step 4: Add the field to STATUS_ACK**

In `payload/src/runtime.c`'s `handle_status_frame`:

Add to the locals snapshot block, alongside `snap_startup_reason`:
```c
    int snap_prior_verdict;
```
Inside the mutex, after `snap_startup_reason = state->startup_reason;`:
```c
    snap_prior_verdict  = state->prior_verdict;
```

In the `snprintf` format string, immediately after the
`"\"takeover_requested\":%d,\"started_at_unix\":%llu,"` line, insert:
```c
                   /* How the PREVIOUS instance ended: "clean",
                    * "killed_externally", "wedged" or "stale". Absent on
                    * older payloads — the client treats that as unknown. */
                   "\"prior_instance\":\"%s\","
```
and add the matching argument immediately after `snap_takeover_req,`:
```c
                   instance_verdict_name(
                       (ps5upload2_prior_verdict_t)snap_prior_verdict),
```

Keep the argument order exactly aligned with the format string — `snprintf`
takes them positionally and a misalignment here corrupts every following field.

- [ ] **Step 5: Build and test**

Run: `make payload && make test-payload`
Expected: clean build, all selftests pass.

- [ ] **Step 6: Verify the JSON shape by eye**

Run:
```bash
grep -n "prior_instance" payload/src/runtime.c
```
Expected: exactly one match, the key spelled `prior_instance` in snake_case.

- [ ] **Step 7: Commit**

```bash
git add payload/include/runtime.h payload/src/runtime.c payload/src/main.c
git commit -m "feat(payload): report how the previous instance ended on STATUS_ACK

Classified once at startup from the ownership record, before it is
overwritten. killed_externally is the SIGKILL/OOM signature and is
otherwise invisible, since the victim cannot log its own death.

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>"
```

---

### Task 6: Neighbour census

**Files:**
- Modify: `payload/include/proc_list.h` (new prototype)
- Modify: `payload/src/proc_list.c` (new function, after `proc_name_by_pid` at line ~361)
- Modify: `payload/src/main.c` (call it after the verdict, line ~454)

**Interfaces:**
- Consumes: `proc_name_is_ours` (Task 1); the existing `KINFO_PID_OFFSET` / `KINFO_TDNAME_OFFSET` constants in `proc_list.c`.
- Produces: `void proc_log_homebrew_neighbours(void)` — writes one summary line plus one line per homebrew-looking process to stderr. No return value; best-effort.

- [ ] **Step 1: Declare it**

In `payload/include/proc_list.h`, add:

```c
/* Log every homebrew-looking process on the console to stderr (and so to
 * stderr.log and the bug bundle).
 *
 * "Homebrew-looking" = a thread name ending in ".elf", which is what every
 * payload loaded through elfldr gets, plus our own "ps5upload*" threads.
 * System processes are named without the extension (SceShellUI,
 * SceRedisServer, ...) and are skipped.
 *
 * This is the data that was missing from every "the helper just dies"
 * report: what ELSE was running, and was anything else wearing the generic
 * "payload.elf" name that the scene's kill-my-predecessor sweeps target.
 * One sysctl, once, at startup. Best-effort — never fails the boot. */
void proc_log_homebrew_neighbours(void);
```

- [ ] **Step 2: Implement it**

In `payload/src/proc_list.c`, add `#include "proc_identity.h"` near the other
includes, then append after `proc_name_by_pid`:

```c
void proc_log_homebrew_neighbours(void) {
    int mib[4] = {CTL_KERN, KERN_PROC, KERN_PROC_PROC, 0};
    size_t buf_size = 0;
    if (sysctl(mib, 4, NULL, &buf_size, NULL, 0) != 0 || buf_size == 0) {
        fprintf(stderr, "[payload2] neighbours: sysctl unavailable\n");
        return;
    }

    /* 25% headroom + 1 KiB padding — same growth strategy the rest of this
     * file uses, because the proc list can grow between the sizing call and
     * the fetch. */
    size_t alloc = buf_size + (buf_size / 4) + 1024;
    uint8_t *kbuf = (uint8_t *)malloc(alloc);
    if (!kbuf) return;

    size_t got = alloc;
    if (sysctl(mib, 4, kbuf, &got, NULL, 0) != 0) {
        free(kbuf);
        fprintf(stderr, "[payload2] neighbours: sysctl fetch failed\n");
        return;
    }

    const size_t MIN_KINFO_BYTES = KINFO_TDNAME_OFFSET + 1;
    int count = 0;
    int generic = 0;

    fprintf(stderr, "[payload2] neighbours: homebrew processes on this console\n");
    for (uint8_t *p = kbuf; (size_t)(p - kbuf) + sizeof(int) <= got;) {
        int ki_structsize = *(int *)p;
        if (ki_structsize <= 0 ||
            (size_t)ki_structsize < MIN_KINFO_BYTES ||
            (size_t)(p - kbuf) + (size_t)ki_structsize > got) {
            break;
        }
        pid_t pid = *(pid_t *)&p[KINFO_PID_OFFSET];
        const char *tdname = (const char *)&p[KINFO_TDNAME_OFFSET];
        size_t name_max = (size_t)ki_structsize - KINFO_TDNAME_OFFSET;

        /* Bounded copy: tdname is not guaranteed NUL-terminated within the
         * record. */
        char name[64] = {0};
        size_t i = 0;
        for (; i < name_max && i + 1 < sizeof(name) && tdname[i]; ++i) {
            name[i] = tdname[i];
        }
        name[i] = '\0';

        p += (size_t)ki_structsize;

        size_t len = strlen(name);
        int is_elf = (len > 4 && strcmp(name + len - 4, ".elf") == 0);
        if (!is_elf && !proc_name_is_ours(name)) continue;

        int mine = proc_name_is_ours(name);
        if (strcmp(name, "payload.elf") == 0) generic++;
        fprintf(stderr, "[payload2]   pid=%d name=%s%s\n",
                (int)pid, name, mine ? " (ours)" : "");
        count++;
    }
    free(kbuf);

    fprintf(stderr, "[payload2] neighbours: %d homebrew process(es)\n", count);
    if (generic > 0) {
        fprintf(stderr,
                "[payload2] neighbours: %d process(es) named 'payload.elf' — any payload "
                "running the scene's kill-my-predecessor-by-name sweep will SIGKILL "
                "them all\n",
                generic);
    }
}
```

Verify `<stdio.h>`, `<stdlib.h>`, `<string.h>`, `<sys/sysctl.h>` and
`<sys/types.h>` are already included at the top of `proc_list.c`; add whichever
are missing.

- [ ] **Step 3: Call it from main()**

In `payload/src/main.c`, immediately after `startup_trace("PRIOR_VERDICT_DONE");`
from Task 5, insert:

```c
    proc_log_homebrew_neighbours();
    startup_trace("NEIGHBOUR_CENSUS_DONE");
```

Verify `#include "proc_list.h"` is present in `main.c`; add it if not.

- [ ] **Step 4: Build and test**

Run: `make payload && make test-payload`
Expected: clean build, all selftests pass.

- [ ] **Step 5: Commit**

```bash
git add payload/include/proc_list.h payload/src/proc_list.c payload/src/main.c
git commit -m "feat(payload): log homebrew neighbours at startup

One sysctl at boot, recording what else is running on the console and
flagging anything wearing the generic payload.elf name. This is the data
that was absent from every 'the helper just dies' report.

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>"
```

---

### Task 7: Last-resort takeover sweep instead of giving up

**Files:**
- Modify: `payload/src/runtime.c` (new `runtime_sweep_our_instances`, after `runtime_reap_prior_instance`)
- Modify: `payload/include/runtime.h` (prototype)
- Modify: `payload/src/main.c:485-492` (the give-up branch)

**Interfaces:**
- Consumes: `proc_name_is_ours` (Task 1); `runtime_system_boottime_unix` (existing static in `runtime.c`).
- Produces: `int runtime_sweep_our_instances(runtime_state_t *state)` — SIGKILLs every `ps5upload*` process except our own, returns the number killed.

- [ ] **Step 1: Declare it**

In `payload/include/runtime.h`:

```c
/* LAST RESORT. SIGKILL every process whose name carries our own
 * "ps5upload" prefix, except this one. Returns how many were killed.
 *
 * Only ever called after BOTH the cooperative TAKEOVER_REQUEST handshake
 * AND the pid-based reap have failed. The graceful path stays primary
 * because it calls runtime_mark_active_transactions(..., "interrupted")
 * first, which tears the journal down cleanly so upload resume survives;
 * a SIGKILL skips all of that.
 *
 * Unlike the pid-based reap this does NOT need an ownership record, which
 * is the case it exists for: a predecessor whose record was lost or
 * overwritten is otherwise unreachable and the new payload just exits. */
int runtime_sweep_our_instances(runtime_state_t *state);
```

- [ ] **Step 2: Implement it**

In `payload/src/runtime.c`, after `runtime_reap_prior_instance`:

```c
int runtime_sweep_our_instances(runtime_state_t *state) {
    (void)state;
    int mib[4] = {CTL_KERN, KERN_PROC, KERN_PROC_PROC, 0};
    size_t buf_size = 0;
    int me = (int)getpid();
    int killed = 0;

    if (sysctl(mib, 4, NULL, &buf_size, NULL, 0) != 0 || buf_size == 0) return 0;
    size_t alloc = buf_size + (buf_size / 4) + 1024;
    uint8_t *kbuf = (uint8_t *)malloc(alloc);
    if (!kbuf) return 0;
    size_t got = alloc;
    if (sysctl(mib, 4, kbuf, &got, NULL, 0) != 0) {
        free(kbuf);
        return 0;
    }

    const size_t MIN_KINFO_BYTES = KINFO_TDNAME_OFFSET + 1;
    for (uint8_t *p = kbuf; (size_t)(p - kbuf) + sizeof(int) <= got;) {
        int ki_structsize = *(int *)p;
        if (ki_structsize <= 0 ||
            (size_t)ki_structsize < MIN_KINFO_BYTES ||
            (size_t)(p - kbuf) + (size_t)ki_structsize > got) {
            break;
        }
        pid_t pid = *(pid_t *)&p[KINFO_PID_OFFSET];
        const char *tdname = (const char *)&p[KINFO_TDNAME_OFFSET];
        size_t name_max = (size_t)ki_structsize - KINFO_TDNAME_OFFSET;
        char name[64] = {0};
        size_t i = 0;
        for (; i < name_max && i + 1 < sizeof(name) && tdname[i]; ++i) {
            name[i] = tdname[i];
        }
        name[i] = '\0';
        p += (size_t)ki_structsize;

        if ((int)pid <= 1 || (int)pid == me) continue;
        if (!proc_name_is_ours(name)) continue;

        fprintf(stderr,
                "[payload2] sweep: SIGKILL pid=%d name=%s (ports still held after "
                "handshake and reap both failed)\n",
                (int)pid, name);
        if (kill(pid, SIGKILL) == 0) killed++;
    }
    free(kbuf);

    if (killed > 0) {
        /* Same confirmation window the pid-based reap uses: give the kernel
         * ~1 s to actually tear the processes down before we retry the bind.
         * A survivor is kernel-wedged and only a reboot clears it. */
        usleep(1000000);
    }
    fprintf(stderr, "[payload2] sweep: killed %d instance(s)\n", killed);
    return killed;
}
```

Verify `KINFO_PID_OFFSET` and `KINFO_TDNAME_OFFSET` are visible in `runtime.c`.
They are defined in `proc_list.c`, so move both `#define`s into
`payload/include/proc_list.h` (above the existing prototypes) and delete them
from `proc_list.c`, leaving the `_Static_assert` at `proc_list.c:53` in place.

- [ ] **Step 3: Use it in the give-up branch**

In `payload/src/main.c`, replace the inner failure block (lines ~485-492) with:

```c
        if (runtime_try_takeover(&state) != 0) {
            /* The handshake AND the pid-based reap have both failed. Before
             * telling the user to restart the console — which is what they
             * had to do until now — sweep for anything wearing our own
             * process-name prefix and SIGKILL it. This catches a predecessor
             * whose ownership record was lost or overwritten, which the
             * pid-based reap cannot see. Only our own prefix is ever matched,
             * never the generic payload.elf. */
            startup_trace("TAKEOVER_FAILED_SWEEPING");
            fprintf(stderr,
                    "takeover and reap both failed — sweeping our own instances\n");
            if (runtime_sweep_our_instances(&state) > 0 &&
                runtime_try_takeover(&state) == 0) {
                startup_trace("TAKEOVER_DONE_AFTER_SWEEP");
            } else {
                /* Ports STILL held after a SIGKILL means the old process is
                 * kernel-wedged (un-killable) — only a reboot clears that. */
                startup_trace("TAKEOVER_FAILED");
                fprintf(stderr,
                        "takeover failed even after sweeping — ports still held\n");
                pop_notification(
                    "PS5Upload: a previous instance is stuck and can't be cleared — please restart the PS5");
                return 1;
            }
        }
```

- [ ] **Step 4: Build and test**

Run: `make payload && make test-payload`
Expected: clean build, all selftests pass.

- [ ] **Step 5: Confirm the sweep is not on the happy path**

Run:
```bash
grep -n "runtime_sweep_our_instances" payload/src/main.c
```
Expected: exactly one call site, inside the nested `if (runtime_try_takeover(&state) != 0)` block — never in the `else` branch that handles a healthy cooperative takeover.

- [ ] **Step 6: Commit**

```bash
git add payload/include/runtime.h payload/include/proc_list.h payload/src/proc_list.c payload/src/runtime.c payload/src/main.c
git commit -m "feat(payload): sweep our own stuck instances before giving up

Previously a second failed takeover exited the process, leaving the user
with a wedged old payload, no new one, and a 'restart the PS5' toast. Now
we SIGKILL anything carrying our own ps5upload prefix and retry the bind
once. Only our own prefix is matched, never the generic payload.elf that
kstuff, SMP and every other payload also wear.

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>"
```

---

### Task 8: Surface the verdict in the client

**Files:**
- Modify: `client/src/api/ps5.ts:4476-4505` (`payloadCheck`)
- Modify: `client/src/state/connection.ts` (runtime + host-status fields, lines ~93, ~108, ~146, ~185, ~207, ~222)
- Modify: `client/src/layout/AppShell.tsx` (lines ~229, ~431 — carry the field like `ucredElevated`)
- Modify: `client/src/screens/Connection/index.tsx` (render the warning)
- Modify: `client/src/lib/diagnosticBundle.ts:56,239`
- Modify: `client/src/lib/ps5Snapshot.ts:59,260`
- Modify: `client/src/i18n/locales/en.ts`
- Modify: `scripts/i18n-known-missing.json`

**Interfaces:**
- Consumes: the STATUS_ACK key `prior_instance` from Task 5. The engine
  (`ps5upload-engine/src/lib.rs:4208`) and the Tauri command
  (`client/src-tauri/src/commands/probes.rs:121`) both pass the status object
  through as an untyped `serde_json::Value`, so **no Rust change is needed**.
- Produces: `priorInstance: string | null` on the connection runtime state.

- [ ] **Step 1: Parse the new field**

In `client/src/api/ps5.ts`, add to the `status` shape inside the `invoke<...>`
type parameter, after `ucred_elevated?: boolean;`:

```ts
      prior_instance?: string;
```

Add to the returned object literal, after the `ucredElevated` entry:

```ts
    priorInstance:
      typeof resp?.status?.prior_instance === "string"
        ? resp.status.prior_instance
        : null,
```

Add to the declared return type of `payloadCheck`, next to `ucredElevated`:

```ts
  /** How the PREVIOUS payload instance ended, straight from STATUS_ACK:
   *  "clean" | "killed_externally" | "wedged" | "stale". null on payloads
   *  older than this field, which is indistinguishable from "unknown". */
  priorInstance: string | null;
```

- [ ] **Step 2: Carry it through the connection store**

In `client/src/state/connection.ts`, mirror every place `ucredElevated`
appears (lines ~93, ~108, ~146, ~185, ~207, ~222) with `priorInstance`:
- the runtime interface: `priorInstance: string | null;`
- its initial value: `priorInstance: null,`
- the host-status interface: `priorInstance: string | null;`
- the carried-key union: add `| "priorInstance"`
- the projection: `priorInstance: rt.priorInstance,`
- the second initial value: `priorInstance: null,`

- [ ] **Step 3: Store it from the poller**

In `client/src/layout/AppShell.tsx`, at line ~229 add `priorInstance: null,` to
the reset object, and at line ~431 mirror the `ucredElevated` carry-over line:

```ts
          priorInstance: carryOver ? prev.priorInstance : s.priorInstance,
```

- [ ] **Step 4: Add the i18n strings**

In `client/src/i18n/locales/en.ts`, add (keeping the file's existing column-0
formatting — do **not** run Prettier over it):

```ts
connection_prior_killed: "The previous helper was killed by something else on your PS5",
connection_prior_killed_detail: "It did not exit on its own. Another payload on the console — or the system running low on memory — ended it. If you load ps5upload through an autoloader, try sending it from here instead.",
```

- [ ] **Step 5: Render it**

In `client/src/screens/Connection/index.tsx`, next to the existing
ucred-elevation warning, render a warning when
`runtime.priorInstance === "killed_externally"`, using
`tr("connection_prior_killed", undefined, "...")` and
`tr("connection_prior_killed_detail", undefined, "...")` with the English
strings above as the inline fallbacks. Follow whatever warning component the
ucred banner already uses on that screen — do not invent a new one.

- [ ] **Step 6: Add it to the diagnostics**

In `client/src/lib/diagnosticBundle.ts`, add `prior_instance: string | null;`
to the type at line ~56 and `prior_instance: conn.priorInstance,` at line ~239.

In `client/src/lib/ps5Snapshot.ts`, add `prior_instance: string | null;` at
line ~59 and `prior_instance: conn.priorInstance ?? null,` at line ~260.

Both use snake_case keys, matching the surrounding entries.

- [ ] **Step 7: Satisfy the i18n coverage gate**

Run:
```bash
node scripts/i18n-coverage.mjs
```
If it reports the two new keys as missing from non-English locales, either
translate them or add both key names to each locale's `missing` array in
`scripts/i18n-known-missing.json`, then re-run until it passes.

- [ ] **Step 8: Run the client tests and lint**

Run:
```bash
make test-client
make lint-client
```
Expected: both pass.

- [ ] **Step 9: Commit**

```bash
git add client/src scripts/i18n-known-missing.json
git commit -m "feat(client): surface an externally-killed previous helper

The engine and the Tauri probe both pass STATUS_ACK through untyped, so
this is a TypeScript-only change. An externally SIGKILLed helper was
previously invisible to the user and to bug reports.

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>"
```

---

### Task 9: Hardware verification

**Files:** none — this is a manual verification pass on real consoles.

**Interfaces:**
- Consumes: everything from Tasks 1-8.
- Produces: a verification note appended to the spec.

- [ ] **Step 1: Deploy to the Pro (FW 9.60)**

Run: `make send-payload PS5_HOST=<pro-ip>`
Then load ps5upload through ps5-payload-manager's autoloader on that console.

- [ ] **Step 2: Confirm the process name**

Open pldmgr's web UI at `http://<pro-ip>:8084`, go to its process list.
Expected: our process appears as `ps5upload.elf`, **not** `payload.elf` and not
`ps5upload-wake`. This is the check that pins the representative-thread
behaviour the spec's F5 describes.

- [ ] **Step 3: Confirm the stdio detachment and census**

Fetch `/data/ps5upload/stderr.log` from the console (FTP, or the client's
diagnostic bundle).
Expected: it now contains the startup `printf` output that previously went to
the launcher's socket (the `[payload2] takeover probe on mgmt port=...` lines),
plus the `[payload2] neighbours:` block listing `pldmgr.elf` and `elfldr.elf`.

- [ ] **Step 4: Confirm external-kill detection**

From pldmgr's process list, kill `ps5upload.elf`. Relaunch it. Fetch
`stderr.log` again.
Expected: `[payload2] prior instance: killed_externally` and the follow-up
explanation line. The Connection screen shows the warning from Task 8.

- [ ] **Step 5: Confirm the sweep recovers a wedged predecessor**

With the payload running, suspend it (`kill -STOP` via pldmgr is not available;
use the client's Send to trigger a takeover against a payload that has been
made unresponsive — e.g. start a large Library scan and send during it). Send a
fresh ELF.
Expected: `stderr.log` shows either a clean `TAKEOVER_DONE`, or the escalation
chain `TAKEOVER_FAILED_ESCALATING` → reap → bind. The new payload comes up
either way; it must not exit with the "restart the PS5" toast.

- [ ] **Step 6: Confirm the happy path is unchanged**

With a healthy payload running, press Send in the client.
Expected: `stderr.log` shows the graceful path only — no
`TAKEOVER_FAILED_SWEEPING` and no `sweep: SIGKILL` lines. This is the
regression that matters: the sweep must never fire on a normal redeploy.

- [ ] **Step 7: Repeat steps 1-6 on the Phat (FW 5.10)**

The kinfo offsets and representative-thread behaviour are the firmware-
dependent parts; both consoles must agree.

- [ ] **Step 8: Record the result**

Append a "Hardware verification" section to
`docs/superpowers/specs/2026-09-19-payload-identity-and-survival-design.md`
recording, for each console and firmware, which steps passed and any that did
not. State plainly if a step could not be exercised rather than implying it
passed.

```bash
git add docs/superpowers/specs/2026-09-19-payload-identity-and-survival-design.md
git commit -m "docs: hardware verification for payload identity and survival

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>"
```

---

## Self-review notes

**Spec coverage.** §1 identity → Tasks 1, 2. §2 stdio → Task 3. §3 verdict →
Tasks 4, 5. §4 census → Task 6. §5 sweep → Task 7. §6 surfacing → Task 8.
Testing section → Tasks 1, 4 (unit) and Task 9 (hardware). The spec's
"Follow-up, not part of this spec" pldmgr PR is deliberately not a task here.

**Known transition gap** (spec §1): a predecessor from a pre-change build still
reports as `payload.elf` and is matched by neither the Task 2 reap nor the Task 7
sweep. This is accepted, not fixed — special-casing `payload.elf` would reopen
the bystander risk. It resolves itself after one redeploy cycle.

**Type consistency.** `proc_name_is_ours` (Tasks 1, 2, 6, 7),
`instance_verdict_classify` / `instance_verdict_name` /
`ps5upload2_prior_verdict_t` (Tasks 4, 5), `runtime_classify_prior_instance`
(Task 5), `runtime_sweep_our_instances` (Task 7), `proc_log_homebrew_neighbours`
(Task 6), JSON key `prior_instance` (Tasks 5, 8), TS field `priorInstance`
(Task 8) — each spelled identically everywhere it appears.

**Ordering constraint.** Task 7 moves `KINFO_PID_OFFSET` / `KINFO_TDNAME_OFFSET`
from `proc_list.c` into `proc_list.h`. Task 6 uses them from inside
`proc_list.c`, where they are visible either way, so Tasks 6 and 7 are
order-independent. Tasks 2, 6 and 7 all depend on Task 1; Task 5 depends on
Task 4; Task 8 depends on Task 5.
