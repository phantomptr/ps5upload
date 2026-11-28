// PS5Upload Play - the log.
// SPDX-License-Identifier: GPL-3.0-or-later
#include "log.hpp"

#include "../net/net.hpp"

#include <cstdarg>
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
pthread_mutex_t lock = PTHREAD_MUTEX_INITIALIZER;
} // namespace

void log_open(const char *path) noexcept
{
    t0 = net::now_us();
    if (!path)
        return;
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
    (void)!write(fd, line, static_cast<std::size_t>(n));
    pthread_mutex_unlock(&lock);
}
} // namespace play
