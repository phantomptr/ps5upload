/* PS5 lacks FreeBSD's F_DUPFD_CLOEXEC command. Serialize the fallback with
 * the wrapped fork entry so Codex's fork/exec children cannot inherit
 * a descriptor between duplication and setting its close-on-exec bit.
 * Direct raw fork/rfork/vfork syscalls and concurrent in-process exec are outside
 * this guarantee; this adapter is not a kernel-atomic replacement.
 */
#include <errno.h>
#include <fcntl.h>
#include <pthread.h>
#include <stdarg.h>
#include <unistd.h>

extern int __real_fcntl(int, int, ...);
static pthread_mutex_t fd_fork_lock = PTHREAD_MUTEX_INITIALIZER;
extern pid_t ps5_raw_fork(void);
pid_t __wrap_fork(void) {
    int error = pthread_mutex_lock(&fd_fork_lock);
    if (error) { errno = error; return -1; }
    pid_t child = ps5_raw_fork();
    error = errno;
    pthread_mutex_unlock(&fd_fork_lock);
    errno = error;
    return child;
}

int __wrap_fcntl(int fd, int command, ...) {
    switch (command) {
    case F_GETFD: case F_GETFL: case F_GETOWN:
        return __real_fcntl(fd, command);
    }
    va_list args;
    va_start(args, command);
    if (command == F_GETLK || command == F_SETLK || command == F_SETLKW ||
        command == F_SETLK_REMOTE || command == F_OGETLK ||
        command == F_OSETLK || command == F_OSETLKW) {
        void *argument = va_arg(args, void *);
        va_end(args);
        return __real_fcntl(fd, command, argument);
    }
    switch (command) {
    case F_DUPFD: case F_SETFD: case F_SETFL: case F_SETOWN: case F_DUP2FD:
    case F_READAHEAD: case F_RDAHEAD: case F_DUPFD_CLOEXEC: case F_DUP2FD_CLOEXEC:
        break;
    default:
        va_end(args);
        errno = EINVAL;
        return -1;
    }
    int argument = va_arg(args, int);
    va_end(args);
    int result = __real_fcntl(fd, command, argument);
    if (result >= 0 || errno != EOPNOTSUPP ||
        (command != F_DUPFD_CLOEXEC && command != F_DUP2FD_CLOEXEC)) return result;
    int error = pthread_mutex_lock(&fd_fork_lock);
    if (error) { errno = error; return -1; }
    result = __real_fcntl(fd, command == F_DUPFD_CLOEXEC ? F_DUPFD : F_DUP2FD, argument);
    if (result >= 0 && __real_fcntl(result, F_SETFD, FD_CLOEXEC) < 0) {
        error = errno;
        close(result);
        result = -1;
    } else {
        error = errno;
    }
    pthread_mutex_unlock(&fd_fork_lock);
    errno = error;
    return result;
}
