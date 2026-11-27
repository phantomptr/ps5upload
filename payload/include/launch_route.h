/* Which launch refusals end launch_title instead of trying the other routes.
 *
 * launch_title (register.c) asks Sony's launcher in-process first
 * (sceSystemServiceLaunchApp). When that call is refused it used to go on to
 * the ShellUI route (a ptrace call into SceShellUI) and then three more
 * in-process launcher calls. The ShellUI route exists for a launcher that will
 * not take the call from our process (the caller-pid check the route was
 * written for) or a firmware without the in-process symbol. It does nothing for
 * a title the console itself will not start, and invoking the launcher through
 * ptrace is what restarts SceShellUI (focus-drop control runs on FW 9.60; a
 * no-user launch on FW 13.60), taking homebrew running inside it down too.
 *
 * Measured 2026-11-27 (Pro, FW 13.60, helper 12.6.37): an installed fake
 * package (PPSA99261, in app.db) was refused with 0x80020016, the ladder went
 * on to the ShellUI route, SceShellUI restarted and shadowmountplus.elf went
 * down with it. The title never started.
 *
 * Refusals that end the launch here (after the caller has checked that the
 * title did not start anyway; a non-zero return can still be a launch that is
 * proceeding, see launch_took_effect):
 *
 *   0x8002xxxx  SCE kernel errors (errno + 0x80020000). The launcher got as far
 *               as asking the kernel to start the program and the kernel said
 *               no (0x80020016 = EINVAL; the same code came back when a
 *               malformed fake package's files were refused). The kernel
 *               gives the same answer whichever process asks.
 *   0x80940033  the launcher's "this console starts no homebrew or fake-package
 *               title right now". The ShellUI route was measured to dispatch
 *               and start nothing for it (FW 13.60 sweep, 2026-09-29), and the
 *               app's repair flow (launchRefusal.ts) needs only this code.
 *
 * Everything else keeps the old ladder: -1 (no in-process symbol), and other
 * 0x8094xxxx launcher codes, some of which are transient (0x8094000F right
 * after a register) and are what the ShellUI retry was written to heal. The
 * no-signed-in-user case is stopped earlier in launch_title.
 *
 * Pure, so payload/tests/launch_route_selftest.c pins it on the host. */
#ifndef PS5UPLOAD2_LAUNCH_ROUTE_H
#define PS5UPLOAD2_LAUNCH_ROUTE_H

#include <stdint.h>

#define LAUNCH_SCE_KERNEL_ERROR_MASK  0xFFFF0000u
#define LAUNCH_SCE_KERNEL_ERROR_BASE  0x80020000u
#define LAUNCH_LNC_HOMEBREW_REFUSED   0x80940033u

/* 1 when the in-process launcher's rc means no other route can start the
 * title, so launch_title reports it instead of trying the ShellUI route. */
static inline int launch_refusal_is_final(int rc) {
    uint32_t code = (uint32_t)rc;
    if (rc >= 0 || rc == -1) return 0;
    if ((code & LAUNCH_SCE_KERNEL_ERROR_MASK) == LAUNCH_SCE_KERNEL_ERROR_BASE) return 1;
    return code == LAUNCH_LNC_HOMEBREW_REFUSED;
}

#endif /* PS5UPLOAD2_LAUNCH_ROUTE_H */
