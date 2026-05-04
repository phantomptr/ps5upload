/*
 * shellui_rpc — call Sony APIs *from inside SceShellUI's process*
 * via ptrace remote-call. This is the mechanism that lets us
 * launch games and read sensors on FW 9.60 without our caller-pid
 * being rejected by Sony's per-API caller-context check.
 *
 * Why ShellUI specifically: Sony's launcher and sensor stubs check
 * `getpid() == SceShellUI.pid` (or a small allow-list of system
 * processes). Running the call via ptrace inside ShellUI satisfies
 * that check natively because the kernel sees ShellUI as the caller.
 *
 * Lifecycle:
 *   1. shellui_rpc_init() — find SceShellUI's pid, resolve the
 *      Sony API addresses inside ShellUI via kernel_dynlib_resolve.
 *      Cached for subsequent calls. Idempotent.
 *   2. shellui_rpc_launch_app(title_id, user_id) — the launcher.
 *   3. shellui_rpc_get_*temp() — the sensors.
 *   4. shellui_rpc_shutdown() — release any cached state.
 *
 * Concurrency: every RPC call serialises on a single mutex because
 * a target process can only be ptrace-attached by one tracer at a
 * time. The ptrace attach/detach pair brackets the actual API call;
 * we don't hold ShellUI suspended across multiple RPCs.
 */

#ifndef PS5UPLOAD2_SHELLUI_RPC_H
#define PS5UPLOAD2_SHELLUI_RPC_H

#include <stdint.h>

/* Initialize the RPC layer — find SceShellUI, resolve symbols.
 * Returns 0 on success, -1 if SceShellUI couldn't be found, -2 if
 * symbol resolution failed. Safe to call multiple times — the
 * first successful call latches the pid + addresses; subsequent
 * calls become no-ops. */
int shellui_rpc_init(void);

/* True if init succeeded and the RPC surface is usable. */
int shellui_rpc_ready(void);

/* PID of the resolved SceShellUI process. 0 if not initialized. */
int shellui_rpc_pid(void);

/* Launch a registered title via ShellUI's
 * `sceLncUtilLaunchApp(title_id, NULL, &param)` where param is a
 * 24-byte LncAppParam (size, user_id, app_opt, crash_report,
 * check_flag) with `user_id` populated from
 * `sceUserServiceGetForegroundUser` (also called remotely).
 *
 * Return codes:
 *    0  — Sony's launcher accepted the call. Game launched.
 *   >0  — Sony returned an error code (0x8094* family for various
 *         launcher errors).
 *   -1  — Pre-dispatch RPC machinery failure: couldn't attach to
 *         ShellUI, scratch mmap failed, symbol unresolved. The
 *         remote function never ran; caller may retry or fall back.
 *   -2  — Soft success: the call WAS dispatched into ShellUI
 *         (pt_continue returned cleanly) but post-call cleanup hit
 *         a race because the launcher signalled ShellUI in a way
 *         that broke our waitpid. The launch most likely
 *         succeeded — caller should treat as success and NOT fall
 *         back to in-process strategies (they would race the
 *         running launch and produce a misleading error). */
/* `user_id_hint`: foreground user id from
 * sceUserServiceGetForegroundUser in the caller's process. If the
 * caller has one already (e.g. `launch_title` eagerly fetches it
 * before either path), pass it here so the launch param block
 * uses a guaranteed-non-zero value. Pass <= 0 to fall back to
 * fetching it via a remote sceUserServiceGetForegroundUser RPC into
 * ShellUI — slightly slower and historically unreliable on the very
 * first launch after a fresh register. */
int shellui_rpc_launch_app(const char *title_id, int user_id_hint);

/* Sensor reads. Each returns 0 on success, -1 on RPC failure or
 * when the underlying Sony stub returned non-zero. The output is
 * written to *out only on success. */
int shellui_rpc_get_cpu_temp(int *out_celsius);
int shellui_rpc_get_soc_temp(int *out_celsius);
int shellui_rpc_get_cpu_freq_hz(long *out_hz);
int shellui_rpc_get_soc_power_mw(uint32_t *out_mw);

/* Install a `.pkg` via ShellUI's `sceAppInstUtilInstallByPackage`,
 * invoked through ptrace remote-call so the caller-pid that Sony's
 * PlayGo subsystem sees is SceShellUI's — same context the system's
 * own Settings → Debug Settings → Install Package menu uses. This
 * is the path that works on FW 9.60+ where direct AppInstUtil calls
 * from our payload's process get rejected with 0x80B22404
 * (SCE_PLAYGO_ERROR_CORE_HTTP_STATUS_CODE_404_NOT_FOUND) at URL
 * pre-flight regardless of cred-forge.
 *
 * Mechanism per remote call:
 *   1. pt_attach to ShellUI.
 *   2. pt_mmap a ~16 KiB scratch region in ShellUI's address space.
 *   3. Build MetaInfo + AppInstPkgInfo + PlayGoInfo + their string
 *      payloads in a local buffer with all pointer fields fixed up
 *      to point at the corresponding offsets WITHIN the scratch
 *      region's eventual address.
 *   4. pt_copyin the local buffer to scratch.
 *   5. pt_call sceAppInstUtilInitialize() — idempotent; treats
 *      "already initialised" returns as success.
 *   6. pt_call sceAppInstUtilInstallByPackage(scratch+meta_off,
 *      scratch+pkginfo_off, scratch+playgo_off).
 *   7. Read rax — install accept (0) or Sony error code.
 *   8. pt_munmap, pt_detach.
 *
 * `url`           HTTP URL or absolute PS5 file path Sony's PlayGo
 *                 should fetch. http://desktop:port/...pkg works in
 *                 ShellUI's process context where it doesn't in ours.
 * `content_id`    36-byte content_id from the PKG header (or empty
 *                 — Sony fills it in the pkg_info struct on return,
 *                 we don't seed it).
 * `title`         Display name shown in PS5 install notifications.
 * `out_err_code`  On return: 0 on accept, or Sony's error code
 *                 (0x80A2FFxx for AppInstaller errors, 0x80B2xxxx
 *                 for PlayGo, 0xE000000x for our own machinery).
 *
 * Returns 0 if ShellUI accepted the install request. Negative on
 * RPC machinery failure (couldn't attach, mmap, copyin, or
 * AppInstUtil isn't loaded into ShellUI's address space). Sony's
 * own non-zero return goes through the `out_err_code` channel with
 * a positive return value as documented in launch_app. */
int shellui_rpc_install_pkg(const char *url,
                             const char *content_id,
                             const char *title,
                             uint32_t *out_err_code);

/* Crash-time recovery: best-effort PT_DETACH any target we have an
 * active attach on. Called from main.c's fatal-signal handler so
 * that if our payload SIGSEGV's mid-RPC we don't leave SceShellUI
 * frozen (which would make the PS5 UI hang until reboot). Safe to
 * call from a signal handler — only does ptrace + a single mutex
 * trylock. If trylock fails (we crashed inside an RPC and we're
 * holding the lock from another thread), we skip the detach
 * because re-entering ptrace from a signal is unsafe; the kernel
 * will eventually clean up our process exit and detach the target
 * anyway, just with a longer "frozen UI" window. */
void shellui_rpc_emergency_detach(void);

#endif /* PS5UPLOAD2_SHELLUI_RPC_H */
