/* A save's param.sfo, read and patched in memory (save_owner.h). No console calls.
 *
 * The format: "\0PSF", u32 version, u32 key table offset, u32 data table offset, u32 entry count,
 * then 16-byte entries {u16 key offset, u16 type, u32 length, u32 capacity, u32 data offset}. */
#include "save_owner.h"

#include <stdio.h>
#include <string.h>

#define SFO_HEAD 20u
#define SFO_ENTRY 16u

static uint32_t le32(const uint8_t *p) {
    return (uint32_t)p[0] | ((uint32_t)p[1] << 8) | ((uint32_t)p[2] << 16) | ((uint32_t)p[3] << 24);
}
static uint16_t le16(const uint8_t *p) { return (uint16_t)(p[0] | (p[1] << 8)); }

typedef struct {
    const char *key; /* inside the buffer, NUL-terminated inside the key table */
    size_t value;    /* offset of the value in the buffer */
    uint32_t len;    /* bytes used */
    uint32_t cap;    /* bytes reserved */
} sfo_entry_t;

/* Entry `i` of a param.sfo, bounds-checked. 1, or 0 when it does not lie inside the buffer. */
static int sfo_entry(const uint8_t *buf, size_t size, uint32_t i, sfo_entry_t *e) {
    uint32_t keys = le32(buf + 8), data = le32(buf + 12);
    const uint8_t *ent = buf + SFO_HEAD + (size_t)i * SFO_ENTRY;
    uint64_t kpos = (uint64_t)keys + le16(ent);
    uint64_t vpos = (uint64_t)data + le32(ent + 12);
    uint32_t len = le32(ent + 4), cap = le32(ent + 8);
    if (kpos >= data || len > cap || vpos > size || cap > size - vpos) return 0;
    if (!memchr(buf + kpos, 0, (size_t)(data - kpos))) return 0;
    e->key = (const char *)(buf + kpos);
    e->value = (size_t)vpos;
    e->len = len;
    e->cap = cap;
    return 1;
}

static uint32_t sfo_count(const uint8_t *buf, size_t size) {
    if (!buf || size < SFO_HEAD || memcmp(buf, "\0PSF", 4) != 0) return 0;
    uint32_t keys = le32(buf + 8), data = le32(buf + 12), count = le32(buf + 16);
    if (count == 0 || count > (size - SFO_HEAD) / SFO_ENTRY) return 0;
    if (keys < SFO_HEAD + (uint64_t)count * SFO_ENTRY || keys >= data || data > size) return 0;
    return count;
}

int save_sfo_valid(const uint8_t *buf, size_t size) {
    uint32_t count = sfo_count(buf, size);
    if (!count) return 0;
    for (uint32_t i = 0; i < count; i++) {
        sfo_entry_t e;
        if (!sfo_entry(buf, size, i, &e)) return 0;
    }
    return 1;
}

static int sfo_find(const uint8_t *buf, size_t size, const char *name, sfo_entry_t *out) {
    uint32_t count = sfo_count(buf, size);
    for (uint32_t i = 0; i < count; i++) {
        sfo_entry_t e;
        if (!sfo_entry(buf, size, i, &e)) return 0;
        if (strcmp(e.key, name) == 0) {
            *out = e;
            return 1;
        }
    }
    return 0;
}

static void sfo_text(const uint8_t *buf, const sfo_entry_t *e, char *out, size_t cap) {
    size_t n = e->len < cap - 1 ? e->len : cap - 1;
    memcpy(out, buf + e->value, n);
    out[n] = 0; /* a value is NUL-padded: the text ends at its first NUL */
}

static uint64_t sfo_number(const uint8_t *buf, const sfo_entry_t *e) {
    uint64_t v = 0;
    uint32_t n = e->len >= 8 ? 8 : (e->len >= 4 ? 4 : 0);
    for (uint32_t i = 0; i < n; i++) v |= (uint64_t)buf[e->value + i] << (8 * i);
    return v;
}

int save_sfo_info(const uint8_t *buf, size_t size, save_sfo_info_t *out) {
    sfo_entry_t e;
    if (!out || !save_sfo_valid(buf, size)) return -1;
    memset(out, 0, sizeof *out);
    if (sfo_find(buf, size, "TITLE_ID", &e)) sfo_text(buf, &e, out->title_id, sizeof out->title_id);
    if (sfo_find(buf, size, "SAVEDATA_DIRECTORY", &e)) sfo_text(buf, &e, out->dir_name, sizeof out->dir_name);
    if (sfo_find(buf, size, "MAINTITLE", &e) || sfo_find(buf, size, "TITLE", &e))
        sfo_text(buf, &e, out->main_title, sizeof out->main_title);
    if (sfo_find(buf, size, "SUBTITLE", &e)) sfo_text(buf, &e, out->sub_title, sizeof out->sub_title);
    if (sfo_find(buf, size, "DETAIL", &e)) sfo_text(buf, &e, out->detail, sizeof out->detail);
    if (sfo_find(buf, size, "SAVEDATA_BLOCKS", &e)) out->blocks = sfo_number(buf, &e);
    if (sfo_find(buf, size, "SYSTEM_BLOCKS", &e)) out->system_blocks = sfo_number(buf, &e);
    if (sfo_find(buf, size, "ACCOUNT_ID", &e) && e.len >= 8) {
        out->account_id = sfo_number(buf, &e);
        out->has_account = 1;
    }
    sfo_entry_t params;
    int has_params = sfo_find(buf, size, "PARAMS", &params);
    if (sfo_find(buf, size, "USER_ID", &e) && e.len >= 4) {
        out->user_id = le32(buf + e.value);
        out->has_user = 1;
    } else if (has_params && params.len >= 8) {
        out->user_id = le32(buf + params.value + 4);
        out->has_user = 1;
    }
    if (sfo_find(buf, size, "SAVEDATA_LIST_PARAM", &e) && e.len >= 4)
        out->list_param = le32(buf + e.value);
    else if (has_params && params.len >= 4)
        out->list_param = le32(buf + params.value);
    return 0;
}

int save_sfo_set_owner(uint8_t *buf, size_t size, uint64_t account_id, uint32_t user_id) {
    sfo_entry_t acct, user;
    if (!save_sfo_valid(buf, size)) return -1;
    if (!sfo_find(buf, size, "ACCOUNT_ID", &acct) || acct.len < 8) return -2;
    size_t user_at;
    if (sfo_find(buf, size, "USER_ID", &user) && user.len >= 4)
        user_at = user.value;
    else if (sfo_find(buf, size, "PARAMS", &user) && user.len >= 8)
        user_at = user.value + 4;
    else
        return -3;
    for (int i = 0; i < 8; i++) buf[acct.value + i] = (uint8_t)(account_id >> (8 * i));
    for (int i = 0; i < 4; i++) buf[user_at + i] = (uint8_t)(user_id >> (8 * i));
    return 0;
}

size_t save_sfo_keys(const uint8_t *buf, size_t size, char *out, size_t cap) {
    size_t at = 0;
    if (!out || cap == 0) return 0;
    out[0] = 0;
    uint32_t count = save_sfo_valid(buf, size) ? sfo_count(buf, size) : 0;
    for (uint32_t i = 0; i < count; i++) {
        sfo_entry_t e;
        if (!sfo_entry(buf, size, i, &e)) break;
        int n = snprintf(out + at, cap - at, "%s%s:%u", at ? " " : "", e.key, (unsigned)e.len);
        if (n < 0 || (size_t)n >= cap - at) {
            out[at] = 0;
            break;
        }
        at += (size_t)n;
    }
    return at;
}
