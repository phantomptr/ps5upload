/* What a disk image says about itself (#963): the game inside an .exfat, .ffpkg (UFS2) or .ffpfs
 * (PFS) found on a share or a drive, read in place for the game library (smbd_library.c), the way
 * the PC app names one (engine/crates/ps5upload-engine/src/collection/identify.rs).
 *
 * Only what is needed is read: the boot sector / superblock / header, the root folder, sce_sys,
 * then sce_sys/param.json (else param.sfo) and sce_sys/icon0.png. A few KB of reads plus the icon,
 * each through the caller's read callback (one SMB read each on a share), never the image's data.
 * The image is told by its magic, not its name.
 *
 * A .ffpfs that holds a single uncompressed .exfat / .ffpkg (a wrapped image) is read through to
 * the image inside. A compressed file (a .ffpfsc) is not decoded here: the daemon carries no
 * inflate (a zlib build crashed the helper, see src/ffpfsc_unpack.c), so it answers
 * "img_compressed" and the library shows the title id from the file name.
 *
 * Every length and pointer comes from the file and is checked against the image before it is
 * read; every buffer is on the heap and bounded (a directory is read at most IMG_DIR_MAX bytes,
 * a param file PKG_INFO_MAX_META, the icon `icon_max`). */
#ifndef PS5UPLOAD_SMBD_IMAGE_H
#define PS5UPLOAD_SMBD_IMAGE_H

#include <stddef.h>
#include <stdint.h>

#include "pkg_info.h"

/* The most bytes of one folder's entries looked through for a name. */
#define IMG_DIR_MAX (1024u * 1024u)

typedef struct {
    pkg_info_t info;  /* from param.json / param.sfo */
    uint8_t *icon;    /* sce_sys/icon0.png (heap; img_meta_free), NULL when there is none or it is over icon_max */
    size_t icon_len;
    char format[8];   /* "exfat", "ufs2" or "pfs": the filesystem read (the inner one of a wrapped image) */
} img_meta_t;

/* Reads the image (`size` bytes through `rd`/`ctx`). NULL on success (out->info filled), or why not:
 *   "img_unknown"     not an exFAT, UFS2 or PFS image
 *   "img_compressed"  its files are compressed (a .ffpfsc)
 *   "img_unsupported" a layout this reader does not take (signed, encrypted or 64-bit-inode PFS,
 *                     a file past the single-indirect blocks of UFS2)
 *   "img_no_param"    no sce_sys/param.json or param.sfo inside
 *   "img_damaged"     a structure points outside the image or does not parse
 *   "img_read"        a read failed (the share dropped): worth trying again later
 *   "img_no_memory"
 * `icon_max` 0 = no icon is read. out is always initialised; free it with img_meta_free. */
const char *img_meta_read(pkg_read_fn rd, void *ctx, uint64_t size, size_t icon_max, img_meta_t *out);

void img_meta_free(img_meta_t *m);

#endif
