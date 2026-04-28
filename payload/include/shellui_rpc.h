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
 * Returns Sony's return code (0 = launched, 0x8094* = various
 * launcher errors). -1 if the RPC machinery itself failed
 * (couldn't attach, symbol missing, etc.). */
int shellui_rpc_launch_app(const char *title_id);

/* Sensor reads. Each returns 0 on success, -1 on RPC failure or
 * when the underlying Sony stub returned non-zero. The output is
 * written to *out only on success. */
int shellui_rpc_get_cpu_temp(int *out_celsius);
int shellui_rpc_get_soc_temp(int *out_celsius);
int shellui_rpc_get_cpu_freq_hz(long *out_hz);
int shellui_rpc_get_soc_power_mw(uint32_t *out_mw);

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
