// PS5Upload Play - the log (a file on the console, stderr elsewhere).
// SPDX-License-Identifier: GPL-3.0-or-later
#pragma once

namespace play
{
// Opens the log at path (moves an old one over 1 MiB to path.old). Null: stderr.
void log_open(const char *path) noexcept;
// Where each long-running thread is (a stage number) and how often it went round, for the
// watchdog that says which one stopped (main.cpp). Cheap: two atomics per thread.
enum class Beat : int
{
    main = 0,
    pad,
    sound,
    voice,
    count_,
};
void beat(Beat who, int stage) noexcept;
// The watchdog's view: the stage and the round count of a thread.
void beat_read(Beat who, int *stage, unsigned *rounds) noexcept;
// A log line without the log's lock (the watchdog, when another thread may hold it).
void log_raw(const char *text) noexcept;
void log(const char *fmt, ...) noexcept __attribute__((format(printf, 1, 2)));
} // namespace play
