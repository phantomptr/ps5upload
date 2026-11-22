#ifndef PS5UPLOAD2_START_TRACE_H
#define PS5UPLOAD2_START_TRACE_H

#include <stddef.h>

/*
 * The helper's start/stop trace: /data/ps5upload/startup.log (#903).
 *
 * A console that panics while a helper starts or stops leaves no kernel log and no stderr tail
 * worth reading: the last buffered lines die with it. This file is written so the last step
 * reached is on the disk when the kernel goes down: every line is one write(2) on an O_APPEND
 * descriptor, then fsync(2). Each line names the process, so two helpers taking over from each
 * other read apart:
 *
 *   1763740756.123 pid=123 v12.6.19 TAKEOVER_DONE
 *   1763740756.130 pid=123 v12.6.19 UNMOUNT_BEGIN /mnt/ps5upload/x force=0
 *
 * For each risky step (an unmount, a remount, an LVD detach, the fan ioctl, a takeover step, a
 * kill) a line goes out BEFORE it and one after, so a crash shows a BEGIN with no END.
 *
 * Over START_TRACE_MAX bytes the file moves to startup.log.old in the same folder (a sibling
 * rename, cross_device.h) and starts again. Before /data/ps5upload exists the lines go to
 * /data/ps5upload_startup.log.
 *
 * No locks and no allocation: callable from any thread, never from a signal handler (snprintf).
 * Only for steps that are rare (start, stop, a mount operation), never a hot path: each line
 * costs an fsync.
 */

#ifndef START_TRACE_MAX
#define START_TRACE_MAX (256 * 1024)
#endif

/* One line: the stage name as given. */
void start_trace(const char *stage);

/* One line, printf-style. Cut at 400 bytes. */
void start_tracef(const char *fmt, ...) __attribute__((format(printf, 1, 2)));

/* The line itself, for the host test: "<sec>.<ms> pid=<pid> v<version> <stage>\n" into buf.
 * Control characters in `stage` become '?', so one call is always exactly one line. Returns the
 * length, or -1 when it does not fit. */
int start_trace_format(char *buf, size_t cap, long long sec, long ms, int pid, const char *version,
                       const char *stage);

/* unmount(2) between an UNMOUNT_BEGIN and an UNMOUNT_END line (`why` names the caller). Returns
 * what unmount returned, errno as it left it. */
int start_trace_unmount(const char *path, int flags, const char *why);

/* Where the lines go, for the host test (NULL: the defaults above). */
void start_trace_set_paths(const char *path, const char *old_path, const char *fallback);

#endif
