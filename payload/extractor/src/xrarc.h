/* Password-protected RAR (RAR 3/4 and RAR5, encrypted data and encrypted headers, single or
 * multi-volume) for libarchive's RAR readers, which cannot decrypt.
 *
 * The volumes are read here and handed to libarchive as one stream (its read callback) that is the
 * same archive without the encryption: encrypted headers decrypted, file data decrypted, the
 * "encrypted" marks taken off the headers (and their checksums redone). libarchive then decodes it as
 * it decodes any RAR. Nothing decrypted is written to disk.
 *
 * Two things cannot be handed over as they are and are left to the caller (xrar.c):
 *   - RAR5 usually stores the checksums of an encrypted file keyed with the password (so they give
 *     nothing away). libarchive cannot check those, so they are made 0 (none) in the stream, and
 *     the caller checks the file it wrote against them (xrarc_ent_t);
 *   - RAR 3/4 with only the data encrypted has no password check: a wrong password shows as damaged
 *     data, which the caller reports as a wrong password. libarchive does not check a stored
 *     (uncompressed) file's CRC once it has its size, so the caller checks every encrypted RAR 3/4
 *     file too. */
#ifndef PS5UPLOAD_XRARC_H
#define PS5UPLOAD_XRARC_H

#include <stddef.h>
#include <stdint.h>

#include "archive.h"

enum {
    XRC_OK = 0,
    XRC_E_PASSWORD_REQUIRED,
    XRC_E_WRONG_PASSWORD,
    XRC_E_MISSING_VOLUME,
    XRC_E_TRUNCATED,
    XRC_E_CORRUPT,
    XRC_E_UNSUPPORTED, /* RAR 1.5/2.0 encryption, an unknown RAR5 encryption version */
    XRC_E_NOMEM,
    XRC_E_OPEN,
    XRC_E_NOT_RAR,
};

/* One file entry (a FILE header that starts a file), in archive order, as libarchive returns them. */
typedef struct xrarc_ent {
    struct xrarc_ent *next;
    int refs;
    int encrypted; /* its data is encrypted */
    int check;     /* the caller checks the file it writes against the checksum below */
    int keyed;     /* that checksum is keyed with `hashkey` (RAR5) */
    int complete;  /* the header of its last part was read: the checksum below is final */
    int have_crc;
    uint32_t crc; /* CRC32 of the unpacked data */
    int have_blake;
    uint8_t blake[32]; /* BLAKE2sp of the unpacked data (keyed) */
    uint8_t hashkey[32];
} xrarc_ent_t;

typedef struct xrarc xrarc_t;

/* Opens the volumes (`vols`, NULL-terminated, kept by the caller) and reads the first volume's
 * headers up to the first encrypted one. `password` may be NULL; it is kept by the caller and must outlive the handle. NULL
 * with *err set on failure (XRC_E_PASSWORD_REQUIRED: the headers are encrypted and there is no
 * password; XRC_E_WRONG_PASSWORD: a password check failed). */
xrarc_t *xrarc_open(char **vols, const char *password, int *err);

/* Whether anything was found encrypted so far (headers, or a file's data); whether the password
 * was confirmed by the archive's own check value (RAR5) or by decrypting headers (RAR 3/4 -hp);
 * whether the archive is solid (1, 0, -1 unknown). */
int xrarc_encrypted(const xrarc_t *t);
int xrarc_confirmed(const xrarc_t *t);
int xrarc_solid(const xrarc_t *t);

/* The first failure (XRC_*), 0 when none. */
int xrarc_error(const xrarc_t *t);

/* Hands the stream to `a` (archive_read_open2 with our callbacks). ARCHIVE_OK or an error. */
int xrarc_attach(xrarc_t *t, struct archive *a);

/* The next file entry, in order; the caller owns one reference (xrarc_release). NULL: none read. */
xrarc_ent_t *xrarc_take(xrarc_t *t);
void xrarc_release(xrarc_t *t, xrarc_ent_t *e);

/* Closes the files and frees everything (the keys too). */
void xrarc_close(xrarc_t *t);

#endif
