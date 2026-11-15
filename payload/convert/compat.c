/* PS5-only adapters outside shim/ (which stays byte-identical to upstream): statfs symvers and the
 * cpuset query below. libc's FreeBSD 11 ABI (--cfg libc_unstable_freebsd_version="11") asks for
 * statfs@FBSD_1.0 and fstatfs@FBSD_1.0; the SDK exports them unversioned, with the same FreeBSD 11
 * struct statfs (MNAMELEN 88), so forwarding is enough, as shim/freebsd11.c does for stat. Plain
 * fstatfs is the shim's --wrap=fstatfs (raw syscall 397, the FreeBSD 11 layout).
 */
#include <sys/param.h>
#include <sys/mount.h>

int ps5_statfs(const char *path, struct statfs *buffer) {
    return statfs(path, buffer);
}
int ps5_fstatfs(int fd, struct statfs *buffer) {
    return fstatfs(fd, buffer);
}
__asm__(".symver ps5_statfs,statfs@FBSD_1.0");
__asm__(".symver ps5_fstatfs,fstatfs@FBSD_1.0");

/* std's available_parallelism asks cpuset_getaffinity(CPU_LEVEL_WHICH, CPU_WHICH_PID, -1) with
 * libc's 256-bit cpuset_t; the PS5 refuses that (ERANGE) and std fell back to sysconf's 16 CPUs,
 * while a payload may run on 5 (mask 0xea00: CPUs 9, 11, 13-15), so every pool ran 3x
 * oversubscribed (a .pkg's Kraken at 9 MB/s with 15 threads, 43 with 4). The thread query with the
 * kernel's 128-bit set works, and a thread starts with its process's mask, so the process query is
 * answered from the calling thread's. */
#include <string.h>
#include <sys/types.h>
int __real_cpuset_getaffinity(int level, int which, id_t id, size_t size, void *mask);
int __wrap_cpuset_getaffinity(int level, int which, id_t id, size_t size, void *mask) {
    enum { WHICH_TID = 1, WHICH_PID = 2, KERNEL_SET = 16 };
    if (which == WHICH_PID && id == (id_t)-1 && size >= KERNEL_SET) {
        memset(mask, 0, size);
        return __real_cpuset_getaffinity(level, WHICH_TID, -1, KERNEL_SET, mask);
    }
    return __real_cpuset_getaffinity(level, which, id, size, mask);
}
