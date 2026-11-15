/* elfldr maps payloads without a kernel executable pathname. Use the real,
 * validated install path recorded by our installer for this process only.
 */
#include <errno.h>
#include <fcntl.h>
#include <limits.h>
#include <stdio.h>
#include <string.h>
#include <sys/types.h>
#include <sys/stat.h>
#include <sys/sysctl.h>
#include <unistd.h>

static char ps5_executable_path[PATH_MAX];
int ps5_init_executable_path(void) {
    int fd = open("/data/ps5-ai-cli/codex-path", O_RDONLY | O_NOFOLLOW);
    if (fd < 0) return -1;
    ssize_t length = read(fd, ps5_executable_path, sizeof(ps5_executable_path) - 1);
    close(fd);
    if (length <= 0 || length == sizeof(ps5_executable_path) - 1) goto invalid;
    ps5_executable_path[length] = '\0';
    if (ps5_executable_path[length - 1] == '\n') ps5_executable_path[--length] = '\0';
    const char *prefix = "/data/pldmgr/payloads/codex-service/";
    if (strncmp(ps5_executable_path, prefix, strlen(prefix)) ||
        strstr(ps5_executable_path, "..") || strchr(ps5_executable_path, '\n')) goto invalid;
    fd = open(ps5_executable_path, O_RDONLY | O_NOFOLLOW);
    if (fd < 0) goto invalid;
    unsigned char header[20];
    struct stat info;
    int valid = fstat(fd, &info) == 0 && S_ISREG(info.st_mode) &&
                read(fd, header, sizeof(header)) == sizeof(header) &&
                memcmp(header, "\177ELF\2\1", 6) == 0 &&
                header[16] == 3 && header[17] == 0 && header[18] == 62 && header[19] == 0;
    close(fd);
    if (!valid) goto invalid;
    return 0;
invalid:
    ps5_executable_path[0] = '\0';
    errno = EINVAL;
    return -1;
}

/* The terminal installer owns this stable runtime path. */
int ps5_set_executable_path(const char *path) {
    if (strcmp(path, "/data/ps5-ai-cli/runtime/codex.elf")) { errno = EINVAL; return -1; }
    int fd = open(path, O_RDONLY | O_NOFOLLOW);
    if (fd < 0) return -1;
    unsigned char header[20]; struct stat info;
    int valid = !fstat(fd, &info) && S_ISREG(info.st_mode) &&
        read(fd, header, sizeof(header)) == sizeof(header) &&
        !memcmp(header, "\177ELF\2\1", 6) && header[16] == 3 && header[17] == 0 &&
        header[18] == 62 && header[19] == 0;
    close(fd);
    if (!valid) { errno = EINVAL; return -1; }
    snprintf(ps5_executable_path, sizeof(ps5_executable_path), "%s", path);
    return 0;
}

extern int __real_sysctl(const int *, u_int, void *, size_t *, const void *, size_t);
int __wrap_sysctl(const int *name, u_int count, void *output, size_t *length,
                  const void *input, size_t input_length) {
    if (ps5_executable_path[0] && count == 4 && name[0] == CTL_KERN &&
        name[1] == KERN_PROC && name[2] == KERN_PROC_PATHNAME &&
        (name[3] == -1 || name[3] == getpid()) && !input && !input_length) {
        if (!length) { errno = EFAULT; return -1; }
        size_t needed = strlen(ps5_executable_path) + 1;
        size_t available = *length;
        *length = needed;
        if (!output) return 0;
        if (available < needed) { errno = ENOMEM; return -1; }
        memcpy(output, ps5_executable_path, needed);
        return 0;
    }
    return __real_sysctl(name, count, output, length, input, input_length);
}
