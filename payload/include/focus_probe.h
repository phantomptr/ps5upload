#ifndef PS5UPLOAD2_FOCUS_PROBE_H
#define PS5UPLOAD2_FOCUS_PROBE_H

#include <stddef.h>

/*
 * Read which application currently owns the screen, WITHOUT ptrace.
 *
 * Why this exists: a game brought to the foreground by the "Bring to
 * front" button drops back to the dashboard after a while, and nothing
 * we sample explains it. ShadowMount+ polls two ShellUI event flags
 * (SceShellCoreUtilAppFocus, SceLncUtilSystemStatus) and BOTH stayed
 * silent across a measured window in which the drop demonstrably
 * happened — so those flags are ShellUI's internal bookkeeping, not a
 * faithful record of what is on screen.
 *
 * sceSystemServiceGetAppIdOfBigApp is the direct question instead: it
 * returns the app id of the full-screen ("big") app that currently owns
 * the display, so comparing it against a game's known app id is an
 * unambiguous foreground/background answer.
 *
 * CRITICAL — no ptrace. Every symbol here is resolved with
 * dlsym(RTLD_DEFAULT, ...) and called IN-PROCESS, the same way
 * runtime.c resolves sceLncUtilKillApp. Reading focus must never
 * ptrace-attach SceShellUI: an attach that stops it and does not
 * resume leaves the console UI frozen, recoverable only by holding the
 * power button. That has happened in this codebase before. If you
 * extend this file, keep it dlsym-only.
 *
 * Response shape:
 *
 *   {"ok":true,
 *    "apis":{"big_app":true,"mini_app":true,"status":false,...},
 *    "big_app_id":24600,"mini_app_id":0,
 *    "monotonic_ms":123456}
 *
 * Availability is reported per-symbol rather than assumed: these are
 * undeclared in the payload SDK and resolve (or not) per firmware, so
 * the caller can tell "not focused" apart from "cannot tell".
 *
 * Returns 0 on success (buf holds valid JSON, *written_out set), non-zero
 * on internal error with a short ASCII code in *err_out.
 */
int focus_probe_get_json(char *buf, size_t cap, size_t *written_out,
                         const char **err_out);

#endif /* PS5UPLOAD2_FOCUS_PROBE_H */
