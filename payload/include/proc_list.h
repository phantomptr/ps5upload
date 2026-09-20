#ifndef PS5UPLOAD2_PROC_LIST_H
#define PS5UPLOAD2_PROC_LIST_H

#include <stddef.h>

/* Layout offsets inside FreeBSD's kinfo_proc as exposed via
 * sysctl(KERN_PROC_PROC). Same offsets shellui_rpc.c uses. */
#define KINFO_PID_OFFSET     72
#define KINFO_TDNAME_OFFSET  447

/*
 * Walk the kernel's allproc linked list and emit a JSON blob describing
 * every process on the system. Response shape:
 *
 *   {"ok":true,"procs":[{"pid":123,"name":"SceShellUI"}, ...]}
 *
 * or, when kernel R/W is unavailable (no kstuff loaded, etc):
 *
 *   {"ok":false,"error":"kernel_rw_unavailable"}
 *
 * This is a simple observability primitive, not a stepping stone to
 * process control. The payload never writes back to the kernel via
 * this path — only reads. The caller (the management-port handler)
 * is responsible for framing the returned bytes into an FTX2 frame.
 *
 * Returns 0 on success (buf contains valid JSON, *written_out set to
 * byte count), non-zero on internal error. `err_out` receives a short
 * ASCII code on failure and is left untouched on success.
 */
int proc_list_get_json(char *buf, size_t cap, size_t *written_out,
                       const char **err_out);

/*
 * Detailed variant for the in-app process manager. Same envelope as
 * proc_list_get_json, but each process object also carries:
 *
 *   {"pid":123,"name":"SceShellUI","comm":"SceShellUI","title_id":"",
 *    "app_id":0,"memory_mib":12.3,"threads":7,"kind":"system"}
 *
 * `kind` is one of "app" (registered game/app, has title_id), "payload"
 * (user .elf homebrew), or "system" (everything else — the UI hides and
 * guards these). Memory is resident set size in MiB; threads is the
 * process thread count. Reads only — no kernel write.
 */
int proc_list_get_json_ex(char *buf, size_t cap, size_t *written_out,
                          const char **err_out);

/*
 * SIGKILL a process by pid. Returns 0 on success, -1 on a guard trip
 * (pid <= 1, or the helper's own pid) or a failed kill. The caller (UI)
 * must confirm before killing a process classified as "system" — this
 * function trusts the request but still refuses self/kernel/init.
 */
int proc_kill(int pid);

/*
 * Look up the command/thread name of a single process by pid via
 * sysctl(KERN_PROC_PID) — same FreeBSD kinfo_proc offsets as the JSON walk.
 * Fills `out` (NUL-terminated) and returns 0 on success; returns non-zero if
 * the process does not exist or the query fails. Used by the takeover path to
 * verify a recorded pid still belongs to *our* payload before killing it
 * (guards against a recycled pid now owned by an unrelated process).
 */
int proc_name_by_pid(int pid, char *out, size_t cap);

/*
 * Find the first process whose thread-name matches `name` via
 * sysctl(KERN_PROC_PROC). Returns the pid (>0) on success, -1 if
 * no match or sysctl fails. No kernel R/W needed.
 */
int proc_find_pid_by_name(const char *name);

/* Pid of a running process whose app TITLE ID matches, or -1.
 *
 * `launch_title` uses this to confirm an attempt worked before trying the
 * next strategy: Sony's launch calls can report failure for a launch that is
 * actually proceeding, and re-launching a title that is already starting
 * makes the shell bounce it to the background. */
int proc_find_pid_by_title_id(const char *title_id);

/* App id of a running title (0 = not running). Sony's focus/kill APIs key on
 * this, which is NOT the pid. */
unsigned int proc_app_id_by_title_id(const char *title_id);

/* Emit the scheduler-visible state of every APP process, as JSON:
 *
 *   [{"pid":154,"title_id":"PPSA23226","app_id":24600,
 *     "stat":2,"runtime_us":123456789,"pctcpu":812,
 *     "nthreads":48,"slptime":0,"swtime":900}, ...]
 *
 * Why this exists: FW 9.60 exports no direct "which app has focus" getter
 * (see focus_probe.h), and the two ShellUI event flags ShadowMount+ polls
 * stay silent through a real foreground->background transition. But a PS5
 * game that loses the screen does not keep running at full speed — the shell
 * suspends it or collapses its CPU budget, which is exactly what the
 * SHELLUI_FG_GAME_BG_CPU_MODE flag names.
 *
 * So `runtime_us` (accumulated CPU microseconds) sampled over time answers
 * the question behaviourally: while a title owns the screen the counter
 * climbs steadily; when it is backgrounded the rate collapses. `stat` catches
 * the stronger case where the process is outright stopped (SSTOP).
 *
 * This reads only sysctl KERN_PROC fields we already walk, so unlike calling
 * an undeclared Sony symbol it carries no crash risk and no firmware
 * dependency.
 *
 * Returns 0 on success, non-zero with a short code in *err_out.
 */
int proc_list_app_states_json(char *buf, size_t cap, size_t *written_out,
                              const char **err_out);

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

#endif /* PS5UPLOAD2_PROC_LIST_H */
