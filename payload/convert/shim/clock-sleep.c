/* The SDK's FreeBSD clock_nanosleep syscall is unavailable on PS5. Rust std
 * uses relative CLOCK_MONOTONIC sleeps. The native nanosleep export supplies
 * that wait, including interruption and remaining-time reporting.
 */
#include <errno.h>
#include <time.h>

int __wrap_clock_nanosleep(clockid_t clock_id, int flags,
                          const struct timespec *request,
                          struct timespec *remaining) {
    if (flags & ~TIMER_ABSTIME) return EINVAL;
    /* Do not silently approximate absolute deadlines or CPU clocks. */
    if (flags || (clock_id != CLOCK_MONOTONIC && clock_id != CLOCK_REALTIME))
        return ENOTSUP;
    if (!request) return EFAULT;
    if (request->tv_sec < 0 || request->tv_nsec < 0 || request->tv_nsec >= 1000000000L)
        return EINVAL;

    /* Unlike nanosleep, clock_nanosleep returns an error number directly and
     * does not change errno. request and remaining may point to the same value.
     */
    int saved_errno = errno;
    int result = nanosleep(request, remaining) == 0 ? 0 : errno;
    errno = saved_errno;
    return result;
}
