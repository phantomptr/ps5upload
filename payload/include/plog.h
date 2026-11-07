/* The helper's log levels: the same model as the app and the engine (ps5upload_core::log).
 *
 * Levels 1 error, 2 warn, 3 info, 4 debug, 5 trace, chosen per category (the same names as the
 * engine's: general, api, connection, helper, rp, pairing, wake, transfer, install, cleanup,
 * files, convert, together, golive, mesh, ui) as a global level plus per-category overrides. The
 * engine sends the setting with the `log.level` management method; a level above info is a raise
 * that ends after `until_s` seconds on this helper's monotonic clock, then everything is back at
 * info. Until the engine says otherwise, every category is at info.
 *
 *   PLOG(PLOG_DEBUG, PLOG_RP, "stage %s", name);
 *
 * writes "[debug:rp] stage ..." to stderr (stderr.log, the bug report's copy) when wanted. While
 * it is not, the cost is one byte load and a compare: the arguments are not even evaluated.
 * Never put a PIN, key, token or password in a line. */
#ifndef PS5UPLOAD_PLOG_H
#define PS5UPLOAD_PLOG_H

#include <stddef.h>
#include <stdint.h>

enum {
    PLOG_ERROR = 1,
    PLOG_WARN = 2,
    PLOG_INFO = 3,
    PLOG_DEBUG = 4,
    PLOG_TRACE = 5,
};

enum {
    PLOG_GENERAL = 0,
    PLOG_API,
    PLOG_CONNECTION,
    PLOG_HELPER,
    PLOG_RP,
    PLOG_PAIRING,
    PLOG_WAKE,
    PLOG_TRANSFER,
    PLOG_INSTALL,
    PLOG_CLEANUP,
    PLOG_FILES,
    PLOG_CONVERT,
    PLOG_TOGETHER,
    PLOG_GOLIVE,
    PLOG_MESH,
    PLOG_UI,
    PLOG_NCATS
};

/* Lines at or below this level are always written (the lowest level in force, never above info). */
extern volatile uint8_t plog_floor;
/* The highest level any category writes now (a cheap first gate; may still say yes for a raise
 * that has just run out, which plog_on catches). */
extern volatile uint8_t plog_ceiling;

/* A line at `lv` in `cat` is wanted now. */
int plog_on(int lv, int cat);
/* Write one line (call through PLOG, which checks first). */
void plog_write(int lv, int cat, const char *fmt, ...) __attribute__((format(printf, 3, 4)));

#define PLOG(lv, cat, ...)                                                   \
    do {                                                                     \
        if ((lv) <= plog_floor || ((lv) <= plog_ceiling && plog_on((lv), (cat)))) \
            plog_write((lv), (cat), __VA_ARGS__);                            \
    } while (0)

/* The category's number for its name (-1 when unknown), and its name. */
int plog_cat_by_name(const char *name, size_t len);
const char *plog_cat_name(int cat);
const char *plog_level_name(int lv);

/* Apply a setting sent by the engine: {"level":n,"cats":{"rp":5,...},"until_s":N}, numbers 1-5.
 * `now_s` is the monotonic clock in seconds (plog_now_s(); a parameter for the tests). An empty
 * body changes nothing. Returns 0, or -1 for a body that is not such an object (nothing changes). */
int plog_apply(const char *body, size_t len, uint64_t now_s);
/* The setting as JSON: {"ok":true,"level":n,"cats":{...},"until_s":N} (until_s 0: no raise).
 * Returns the length written, or -1 when it does not fit. */
int plog_describe(char *out, size_t cap, uint64_t now_s);
/* Back to info everywhere when the raise has run out (plog_on calls it). */
void plog_expire(uint64_t now_s);
uint64_t plog_now_s(void);
/* `msg` with a pairing PIN ("PIN: 12345678", four or more digits after "PIN") replaced by
 * "(shown on the TV)": the copy a log or the notification ring keeps. */
void plog_without_pin(const char *msg, char *out, size_t cap);
/* For the tests: back to the start (info everywhere, no raise). */
void plog_reset(void);

#endif
