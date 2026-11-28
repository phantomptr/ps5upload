// PS5Upload Play - the log (a file on the console, stderr elsewhere).
// SPDX-License-Identifier: GPL-3.0-or-later
#pragma once

namespace play
{
// Opens the log at path (moves an old one over 1 MiB to path.old). Null: stderr.
void log_open(const char *path) noexcept;
void log(const char *fmt, ...) noexcept __attribute__((format(printf, 1, 2)));
} // namespace play
