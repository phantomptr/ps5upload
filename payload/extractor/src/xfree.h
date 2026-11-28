/* Freeing an archive's space while it is extracted ("free": true in the request).
 *
 * The archive is deleted as it is unpacked, so a game needs about its own size of free space
 * instead of the archive's and its own. It is asked for by name and never the default: the archive
 * can't be used again.
 *
 * What can be freed, and when:
 *
 * - A set of parts (`x.part1.rar`..., `x.rar` + `x.r00`..., `x.7z.001`..., `x.zip.001`...) is freed
 *   part by part ("parts"): a part is deleted once nothing still to be unpacked needs a byte of it,
 *   and everything unpacked from it is on the drive (each file flushed, its folder flushed).
 *   The unpack is cut into units that decode on their own: a file of a zip or a tar or of a RAR
 *   that is not solid, a block of a 7z (all the files packed in one stream). A part is freed only
 *   when every unit with a byte in it is done, so a solid block spanning three parts keeps all
 *   three until the whole block is out.
 * - Everything else ("end"): one file, a solid RAR (one stream from the first file to the last),
 *   an encrypted RAR (read through xrarc, which hides where each part ends), a single compressed
 *   stream. The archive is deleted when every file is out and on the drive. That saves the step
 *   of deleting it by hand, not room while it unpacks.
 *
 * Before a part goes, the whole archive is read through once without writing anything, so damage
 * anywhere in it is found while it is still whole ("checking"). That pass also tells where each
 * unit's bytes start, which the schedule below is built from, and how much room the unpack needs at
 * most: the space check after it is exact, not a guess.
 *
 * When it stops part way (a failure, a Stop) after a part went, nothing more is deleted, everything
 * unpacked so far stays, and the parts left are renamed `<name>.partial-deleted` (in their own
 * folder: never a rename across drives) so a set without its first part never looks like one that
 * can be opened. Before the first part went, the archive is whole and the job can simply be run
 * again.
 *
 * Plain C, no PS5 types: the schedule functions are tested on their own (tests/xfree_test.c). */
#ifndef PS5UPLOAD_XFREE_H
#define PS5UPLOAD_XFREE_H

#include <stddef.h>
#include <stdint.h>

#include "xjob.h"

#ifdef __cplusplus
extern "C" {
#endif

/* Added to the name of a part left when the unpack stopped after others were deleted. */
#define XF_PARTIAL_SUFFIX ".partial-deleted"

/* "No byte read": a unit that read nothing of the archive (folders, empty files). */
#define XF_NO_READ UINT64_MAX

typedef struct xfvols {
    unsigned count;
    char **path;     /* each part, first to last */
    uint64_t *size;  /* its size when checked */
    uint64_t *start; /* where its first byte is in the set read as one stream */
    uint64_t total;
    unsigned freed;  /* parts 0..freed-1 are deleted */
    int stuck;       /* a delete failed: nothing more is deleted until the end */
    /* Closes the reader's own handle on part `vol` before it is deleted (a deleted file that is
     * still open keeps its space). May be NULL. */
    void (*drop)(void *ctx, unsigned vol);
    void *drop_ctx;
} xfvols_t;

/* ---- the schedule (pure) ---- */

/* How many leading parts end at or before stream offset `at`: those nothing at or after `at` needs. */
unsigned xf_before(const xfvols_t *v, uint64_t at);

/* need[u]: the lowest offset any unit from u on reads (XF_NO_READ when none of them reads). Units
 * run in order; before unit u starts, every part that ends at or before need[u] can go. */
void xf_need(const uint64_t *lo, size_t n, uint64_t *need);

/* The most room the unpack takes at once on the archive's own drive, beyond what is free when it
 * starts: what is written by the end of each unit, less the parts deleted before it began. `bytes`
 * is what each unit writes. */
uint64_t xf_peak(const xfvols_t *v, const uint64_t *need, const uint64_t *bytes, size_t n);

/* ---- the parts on the drive ---- */

/* The parts of the archive, checked for deleting: plain files with one name, in a folder that can
 * be written. 0, or -1 with the cause set (fs_extract_free_refused: ...). `paths` is copied. */
int xf_load(xjob_t *j, xfvols_t *v, char *const *paths, unsigned count);
void xf_free_vols(xfvols_t *v);

/* Whether the archive's first part and the destination are on one drive (only then does its freed
 * space make room for the unpack). Unknown counts as not. */
int xf_same_drive(const char *archive, const char *dest);

/* Deletes parts freed..upto-1, after flushing everything unpacked so far (xs_barrier). A part that
 * can't be deleted stops further deletes (v->stuck) without failing the unpack. -1 only when the
 * flush failed (cause set): then nothing was deleted. */
int xf_release(xjob_t *j, xfvols_t *v, unsigned upto);

/* The unpack finished: everything flushed, then every part left deleted. A part that could not be
 * deleted is counted in j->free_not_removed. 0, or -1 when the flush failed (nothing deleted). */
int xf_finish(xjob_t *j, xfvols_t *v);

/* The unpack stopped after parts were deleted: the parts left are renamed with XF_PARTIAL_SUFFIX
 * and counted (j->parts_left, j->left_first). Nothing when no part was deleted. */
void xf_abandon(xjob_t *j, xfvols_t *v);

/* Tests only (the host build, XTEST_FREE_TRACE=<file>): one line per file committed ("file <rel>")
 * and per part deleted ("freed <name>"), in the order they happen. Nothing on the PS5. */
void xf_trace(const char *what, const char *name);

#ifdef __cplusplus
}
#endif

#endif
