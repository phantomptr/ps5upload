/* The bytes statvfs says a writer may still take, for every "is there room" check.
 *
 * The kernel's count (statfs f_bavail) is SIGNED: it goes below zero once a drive is written into
 * its reserve, which a process running as root (the helper, the extractor) is allowed to do.
 * statvfs hands the same number over as an unsigned one, so a drive that is past full read as
 * about 16 EiB free: every space check passed, and a long write went on through the reserve until
 * the drive had nothing left at all (#869: an extraction "filled the drive at 99%"). Zero or below
 * is no room. Pure, so the host self-test covers it. */
#ifndef PS5UPLOAD_STATVFS_AVAIL_H
#define PS5UPLOAD_STATVFS_AVAIL_H

#include <stdint.h>
#include <sys/statvfs.h>

static inline uint64_t statvfs_avail_blocks_to_bytes(uint64_t bavail, uint64_t block_size) {
    if ((int64_t)bavail <= 0) return 0;
    return bavail * block_size;
}

static inline uint64_t statvfs_avail_bytes(const struct statvfs *v) {
    return statvfs_avail_blocks_to_bytes((uint64_t)v->f_bavail, (uint64_t)(v->f_frsize ? v->f_frsize : v->f_bsize));
}

#endif
