/* save_sfo.c: reading a save's param.sfo and writing its owner (save_owner.h). Host build. */
#include "save_owner.h"

#include <stdio.h>
#include <string.h>

static int g_fail;
static void check(int ok, const char *what) {
    if (!ok) {
        fprintf(stderr, "FAIL: %s\n", what);
        g_fail = 1;
    }
}

static void put32(unsigned char *p, unsigned v) {
    for (int i = 0; i < 4; i++) p[i] = (unsigned char)(v >> (8 * i));
}

typedef struct {
    const char *key;
    const void *val;
    unsigned len, cap;
} field_t;

/* A param.sfo of `n` fields. Returns its size. */
static size_t build(unsigned char *out, size_t cap, const field_t *f, unsigned n) {
    size_t keys = 20 + (size_t)n * 16, klen = 0, dlen = 0;
    for (unsigned i = 0; i < n; i++) klen += strlen(f[i].key) + 1;
    size_t data = keys + ((klen + 3) & ~(size_t)3);
    for (unsigned i = 0; i < n; i++) dlen += f[i].cap;
    if (data + dlen > cap) return 0;
    memset(out, 0, data + dlen);
    memcpy(out, "\0PSF", 4);
    put32(out + 4, 0x101);
    put32(out + 8, (unsigned)keys);
    put32(out + 12, (unsigned)data);
    put32(out + 16, n);
    size_t ko = 0, vo = 0;
    for (unsigned i = 0; i < n; i++) {
        unsigned char *e = out + 20 + (size_t)i * 16;
        e[0] = (unsigned char)ko;
        e[1] = (unsigned char)(ko >> 8);
        e[2] = 4;
        e[3] = 4;
        put32(e + 4, f[i].len);
        put32(e + 8, f[i].cap);
        put32(e + 12, (unsigned)vo);
        memcpy(out + keys + ko, f[i].key, strlen(f[i].key) + 1);
        memcpy(out + data + vo, f[i].val, f[i].len);
        ko += strlen(f[i].key) + 1;
        vo += f[i].cap;
    }
    return data + dlen;
}

int main(void) {
    unsigned char buf[4096], params[1024] = {0};
    const unsigned char account[8] = {0x5c, 0x20, 0xe2, 0xa9, 0x99, 0x6e, 0x35, 0x7a};
    const unsigned char blocks[8] = {48, 0, 0, 0, 0, 0, 0, 0};
    put32(params + 4, 505721148u); /* the user id inside PARAMS */
    params[8] = 0xaa;
    const field_t ps5[] = {
        {"ACCOUNT_ID", account, 8, 8},
        {"MAINTITLE", "Wanderer", 9, 128},
        {"PARAMS", params, 1024, 1024},
        {"SAVEDATA_BLOCKS", blocks, 8, 8},
        {"SAVEDATA_DIRECTORY", "sce_sdmemory", 13, 32},
        {"TITLE_ID", "PPSA26344", 10, 12},
    };
    size_t n = build(buf, sizeof buf, ps5, 6);
    save_sfo_info_t in;
    check(n > 0 && save_sfo_valid(buf, n), "a built param.sfo is valid");
    check(save_sfo_info(buf, n, &in) == 0, "its fields are read");
    check(strcmp(in.title_id, "PPSA26344") == 0 && strcmp(in.dir_name, "sce_sdmemory") == 0, "title and folder");
    check(strcmp(in.main_title, "Wanderer") == 0 && in.blocks == 48, "name and blocks");
    check(in.has_account && in.account_id == 0x7a356e99a9e2205cULL, "account id, little-endian");
    check(in.has_user && in.user_id == 505721148u, "user id from PARAMS+4");

    check(save_sfo_set_owner(buf, n, 0x1122334455667788ULL, 353137057u) == 0, "owner written");
    check(save_sfo_info(buf, n, &in) == 0 && in.account_id == 0x1122334455667788ULL && in.user_id == 353137057u,
          "owner read back");
    check(strcmp(in.title_id, "PPSA26344") == 0 && in.blocks == 48, "the other fields are untouched");
    unsigned char *p = buf + 20 + 6 * 16; /* the key table follows the index */
    check(memcmp(p, "ACCOUNT_ID", 10) == 0, "the key table is untouched");
    /* PARAMS keeps everything but the user id. */
    save_sfo_info_t again;
    check(save_sfo_info(buf, n, &again) == 0 && again.list_param == 0, "PARAMS' first word is kept");

    /* A USER_ID field wins over PARAMS. */
    unsigned char uid_le[4];
    put32(uid_le, 7);
    const field_t with_user[] = {
        {"ACCOUNT_ID", account, 8, 8},
        {"PARAMS", params, 1024, 1024},
        {"USER_ID", uid_le, 4, 4},
    };
    n = build(buf, sizeof buf, with_user, 3);
    check(save_sfo_set_owner(buf, n, 1, 99) == 0 && save_sfo_info(buf, n, &in) == 0 && in.user_id == 99, "USER_ID is the user field");

    /* Refusals. */
    const field_t no_account[] = {{"PARAMS", params, 1024, 1024}, {"TITLE_ID", "PPSA26344", 10, 12}};
    n = build(buf, sizeof buf, no_account, 2);
    check(save_sfo_set_owner(buf, n, 1, 2) == -2, "no ACCOUNT_ID: refused");
    const field_t no_user[] = {{"ACCOUNT_ID", account, 8, 8}, {"TITLE_ID", "PPSA26344", 10, 12}};
    n = build(buf, sizeof buf, no_user, 2);
    check(save_sfo_set_owner(buf, n, 1, 2) == -3, "no user field: refused");
    memset(buf, 0, sizeof buf);
    check(!save_sfo_valid(buf, 3072) && save_sfo_set_owner(buf, 3072, 1, 2) == -1, "the all-zero placeholder is not a param.sfo");

    /* Damaged input never reads outside the buffer. */
    n = build(buf, sizeof buf, ps5, 6);
    check(!save_sfo_valid(buf, n - 1), "a truncated file is refused");
    put32(buf + 16, 0x7fffffff);
    check(!save_sfo_valid(buf, n), "an entry count past the file is refused");
    n = build(buf, sizeof buf, ps5, 6);
    put32(buf + 20 + 12, 0xfffffff0u);
    check(!save_sfo_valid(buf, n), "a value past the file is refused");
    n = build(buf, sizeof buf, ps5, 6);
    put32(buf + 20 + 4, 4096);
    check(!save_sfo_valid(buf, n), "a length over its capacity is refused");

    char keys[256];
    n = build(buf, sizeof buf, with_user, 3);
    save_sfo_keys(buf, n, keys, sizeof keys);
    check(strcmp(keys, "ACCOUNT_ID:8 PARAMS:1024 USER_ID:4") == 0, "the field list");
    char tiny[12];
    save_sfo_keys(buf, n, tiny, sizeof tiny);
    check(strlen(tiny) < sizeof tiny, "the field list stops at its buffer");

    if (!g_fail) printf("save_sfo selftest: ok\n");
    return g_fail;
}
