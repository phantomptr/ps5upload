#ifndef PS5UPLOAD_ATOMIC_FILE_H
#define PS5UPLOAD_ATOMIC_FILE_H

/* Replacing a file so that a crash or a power cut leaves the old one or the new one, never a short one.
 * Writing a sibling temp and renaming it is only half of that: without fsync the rename can reach the
 * disk before the bytes do, and the old file is then gone for an empty one. So the bytes go to disk
 * first, every step is checked, and nothing is renamed after a failure. The temp must be a sibling of
 * the target: rename across mounts panics the kernel (cross_device.h). */
#include <errno.h>
#include <fcntl.h>
#include <stddef.h>
#include <stdio.h>
#include <sys/types.h>
#include <unistd.h>

/* Finish a temp written through stdio. Closes it either way. 0 only when every byte written so far,
 * earlier failed fprintf calls included, is on disk. */
static inline int atomic_file_close_stream(FILE *f) {
    int rc = 0;
    if (fflush(f) != 0 || ferror(f)) rc = -1;
    if (rc == 0 && fsync(fileno(f)) != 0) rc = -1;
    if (fclose(f) != 0) rc = -1;
    return rc;
}

/* Replace `path` with these bytes through the sibling `tmp`. 0 on success. On failure `path` is as it
 * was and `tmp` is gone. O_NOFOLLOW: a link at the temp's name is refused, not written through. */
static inline int atomic_file_write(const char *path, const char *tmp, const void *data, size_t n,
                                    mode_t mode) {
    int fd = open(tmp, O_WRONLY | O_CREAT | O_TRUNC | O_NOFOLLOW, mode);
    if (fd < 0) return -1;
    const unsigned char *p = (const unsigned char *)data;
    size_t off = 0;
    int rc = 0;
    while (off < n) {
        ssize_t w = write(fd, p + off, n - off);
        if (w < 0 && errno == EINTR) continue;
        if (w <= 0) {
            rc = -1;
            break;
        }
        off += (size_t)w;
    }
    if (rc == 0 && fsync(fd) != 0) rc = -1;
    if (close(fd) != 0) rc = -1;
    if (rc == 0 && rename(tmp, path) != 0) rc = -1;
    if (rc != 0) (void)unlink(tmp);
    return rc;
}

#endif /* PS5UPLOAD_ATOMIC_FILE_H */
