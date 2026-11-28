// PS5Upload Play - the log.
// SPDX-License-Identifier: GPL-3.0-or-later
#include "log.hpp"

#include "../net/net.hpp"

#include <atomic>
#include <cerrno>
#include <cstdarg>
#include <cstring>
#include <cstdio>
#include <fcntl.h>
#include <pthread.h>
#include <sys/stat.h>
#include <unistd.h>

namespace play
{
namespace
{
int fd = 2;
std::uint64_t t0 = 0;
char log_path[256] = {};
unsigned reopened = 0;

// The log's own file went away under us (another part closed the descriptor, or it was replaced):
// open it again and say so, with what the write said (a quiet log otherwise hides the rest).
void write_line(const char *line, std::size_t n) noexcept
{
    if (write(fd, line, n) == static_cast<long>(n) || !log_path[0] || reopened > 20)
        return;
    const int err = errno;
    const int f = open(log_path, O_WRONLY | O_CREAT | O_APPEND, 0644);
    if (f < 0)
        return;
    fd = f;
    ++reopened;
    char note[96];
    const int k = std::snprintf(note, sizeof note, "[log] reopened (fd %d, errno %d)\n", f, err);
    (void)!write(fd, note, static_cast<std::size_t>(k));
    (void)!write(fd, line, n);
}
pthread_mutex_t lock = PTHREAD_MUTEX_INITIALIZER;
std::atomic<int> stages[static_cast<int>(Beat::count_)];
std::atomic<unsigned> rounds_[static_cast<int>(Beat::count_)];
} // namespace

void beat(Beat who, int stage) noexcept
{
    const int i = static_cast<int>(who);
    stages[i].store(stage, std::memory_order_relaxed);
    if (stage == 0)
        rounds_[i].fetch_add(1, std::memory_order_relaxed);
}

void beat_read(Beat who, int *stage, unsigned *rounds) noexcept
{
    const int i = static_cast<int>(who);
    *stage = stages[i].load(std::memory_order_relaxed);
    *rounds = rounds_[i].load(std::memory_order_relaxed);
}

void log_raw(const char *text) noexcept
{
    write_line(text, std::strlen(text));
}

void log_open(const char *path) noexcept
{
    t0 = net::now_us();
    if (!path)
        return;
    std::snprintf(log_path, sizeof log_path, "%s", path);
    struct stat st{};
    if (stat(path, &st) == 0 && st.st_size > (1 << 20))
    {
        char old[256];
        std::snprintf(old, sizeof old, "%s.old", path);
        unlink(old);
        rename(path, old);
    }
    const int f = open(path, O_WRONLY | O_CREAT | O_APPEND, 0644);
    if (f >= 0)
        fd = f;
}

void log(const char *fmt, ...) noexcept
{
    char line[1024];
    const std::uint64_t t = net::now_us() - t0;
    int n = std::snprintf(line, sizeof line, "[%8.3f] ", static_cast<double>(t) / 1e6);
    va_list ap;
    va_start(ap, fmt);
    n += std::vsnprintf(line + n, sizeof line - static_cast<std::size_t>(n) - 2, fmt, ap);
    va_end(ap);
    if (n > static_cast<int>(sizeof line) - 2)
        n = sizeof line - 2;
    line[n++] = '\n';
    pthread_mutex_lock(&lock);
    write_line(line, static_cast<std::size_t>(n));
    pthread_mutex_unlock(&lock);
}
} // namespace play
