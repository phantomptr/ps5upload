/* PS5 uses the old FreeBSD stat layout but exports unversioned symbols. */
#include <pthread.h>
#include <pthread_np.h>
#include <errno.h>
#include <stdlib.h>
#include <sys/stat.h>
#include <dirent.h>
#include <limits.h>
#include <signal.h>
#include <sys/event.h>
#include <sys/socket.h>
#include <unistd.h>
#include "compat.h"

int ps5_freebsd11_stat(const char *path, struct stat *buffer) {
    return stat(path, buffer);
}
int ps5_freebsd11_fstat(int fd, struct stat *buffer) {
    return fstat(fd, buffer);
}
__asm__(".symver ps5_freebsd11_stat,stat@FBSD_1.0");
__asm__(".symver ps5_freebsd11_fstat,fstat@FBSD_1.0");

int ps5_freebsd11_lstat(const char *path, struct stat *buffer) {
    return lstat(path, buffer);
}
int ps5_freebsd11_fstatat(int fd, const char *path, struct stat *buffer, int flags) {
    return fstatat(fd, path, buffer, flags);
}
struct dirent *ps5_freebsd11_readdir(DIR *directory) {
    return readdir(directory);
}
int ps5_freebsd11_kevent(int fd, const struct kevent *changes, int nchanges,
                        struct kevent *events, int nevents,
                        const struct timespec *timeout) {
    return kevent(fd, changes, nchanges, events, nevents, timeout);
}
__asm__(".symver ps5_freebsd11_lstat,lstat@FBSD_1.0");
__asm__(".symver ps5_freebsd11_fstatat,fstatat@FBSD_1.1");
__asm__(".symver ps5_freebsd11_readdir,readdir@FBSD_1.0");
__asm__(".symver ps5_freebsd11_kevent,kevent@FBSD_1.0");

/* Assembly trampolines preserve the kernel carry flag and translate errors. */
long ps5_syscall_error(int error) { errno = error; return -1; }

/* PS5 repurposes the FreeBSD syscall numbers for these advisory operations.
 * POSIX specifies returning the error number directly for these two APIs.
 */
int __wrap_posix_fadvise(int fd, off_t offset, off_t length, int advice) {
    (void)fd; (void)offset; (void)length; (void)advice;
    return EOPNOTSUPP;
}
int __wrap_posix_fallocate(int fd, off_t offset, off_t length) {
    (void)fd; (void)offset; (void)length;
    return EOPNOTSUPP;
}

int killpg(pid_t group, int signal_number) {
    if (group < 0) { errno = EINVAL; return -1; }
    return kill(-group, signal_number);
}

/* Rust's FreeBSD backend uses the newer spelling; PS5 exports the old one. */
int pthread_setname_np(pthread_t thread, const char *name) {
    pthread_set_name_np(thread, name);
    return 0;
}

/* Use the platform CSPRNG; never fall back to time or a deterministic seed. */
int getentropy(void *buffer, size_t size) {
    if (size > 256) {
        errno = EIO;
        return -1;
    }
    if (size != 0) arc4random_buf(buffer, size);
    return 0;
}

/* FreeBSD flags: GRND_NONBLOCK=1, GRND_RANDOM=2. The native CSPRNG supplies
 * cryptographic bytes for both modes; no weak fallback or synthetic success.
 */
ssize_t getrandom(void *buffer, size_t size, unsigned flags) {
    if ((flags & ~3u) != 0 || size > SSIZE_MAX) { errno = EINVAL; return -1; }
    if (size != 0) arc4random_buf(buffer, size);
    return (ssize_t)size;
}

#include "getpeereid.c"
#include "lockf.c"
#include "terminal.c"
#include "backtrace-unavailable.c"
#include "fd-compat.c"
#include "executable-path.c"
#include "clock-sleep.c"
#include "string-compat.c"
