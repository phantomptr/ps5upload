/* The AVA1 event log: /data/ps5upload/ava/events.log, a human-readable line per job event
 * (open, resume, done, fail) that a bug report can read with fs.read (P3 Task 9). The
 * the old transaction logs (tx/events.log, tx_*.json) it replaces no longer exist; the job
 * journals under ava/jobs are binary and say nothing a person can read.
 *
 * Rolling: when the file would pass ava1_events_limit (1 MiB), it is renamed to
 * <path>.old (replacing the previous one) and a new file starts, so the pair holds
 * between 1 and 2 MiB of the most recent events. The rename stays inside one directory
 * (never across mounts). Best effort: a log that cannot be written is silently skipped,
 * it never fails or slows a job (one append per event, no fsync). */
#ifndef AVA1_EVENTS_H
#define AVA1_EVENTS_H

#include <stddef.h>
#include <stdint.h>

struct ava1_job;

#define AVA1_EVENTS_LIMIT (1024u * 1024u)
#define AVA1_EVENTS_LINE_MAX 512u

/* Where the log lives; NULL (the default) turns logging off. */
void ava1_events_set_path(const char *path);
/* Tests: the roll size (0 = AVA1_EVENTS_LIMIT). */
void ava1_events_set_limit(uint32_t bytes);
/* Appends "<UTC time> <line>\n"; a line longer than AVA1_EVENTS_LINE_MAX is cut. */
void ava1_log_event(const char *line);
/* "<what> job=<first 4 id bytes in hex> kind=K status=S files=F bytes=B lanes=L [msg]". */
void ava1_log_job_event(const char *what, const struct ava1_job *j, uint16_t status);

#endif
