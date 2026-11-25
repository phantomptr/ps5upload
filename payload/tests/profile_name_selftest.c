/* Host tests for renaming an account slot (profile.c profile_slot_set_name), against a registry
 * that behaves like the console's for the one rule that broke #940: a string write must pass the
 * size the entity is registered with, or it is answered 0x800D0208 (SCE_REGMGR_ERROR_PRM_SIZE).
 * A name that fits is written whole and zero-padded; one that does not is refused with nothing
 * written, never cut. Build: see the root Makefile test-payload. */
#include <stdint.h>
#include <stdio.h>
#include <string.h>

#include "../src/profile.c"

static int failures = 0;

#define CHECK(expr)                                                 \
    do {                                                            \
        if (!(expr)) {                                              \
            fprintf(stderr, "FAIL line %d: %s\n", __LINE__, #expr); \
            failures++;                                             \
        }                                                           \
    } while (0)

/* ---- the registry: the name and type entities of every slot, both registered with 17 ---- */

#define REG_PRM_SIZE 0x800D0208u
#define REG_PRM_REGID 0x800D0203u
#define ENTITY_SIZE 17

static char g_name[PROFILE_SLOT_COUNT + 1][ENTITY_SIZE];
static char g_type[PROFILE_SLOT_COUNT + 1][ENTITY_SIZE];
static int g_writes;
static size_t g_last_len;

static char *entity(uint32_t key) {
    for (int s = 1; s <= PROFILE_SLOT_COUNT; s++) {
        if (key == 125829632u + (uint32_t)(s - 1) * 65536u) return g_name[s];
        if (key == 125874183u + (uint32_t)(s - 1) * 65536u) return g_type[s];
    }
    return NULL;
}

int sys_registry_set_str(uint32_t key, const char *buf, size_t len, uint32_t *err) {
    char *e = entity(key);
    uint32_t rc = !e ? REG_PRM_REGID : len != ENTITY_SIZE ? REG_PRM_SIZE : 0;
    g_last_len = len;
    if (err) *err = rc;
    if (rc) return -1;
    g_writes++;
    memcpy(e, buf, ENTITY_SIZE); /* the console reads the whole field from the buffer */
    return 0;
}
int sys_registry_get_str(uint32_t key, char *buf, size_t cap, uint32_t *err) {
    char *e = entity(key);
    if (err) *err = e ? 0 : REG_PRM_REGID;
    if (!e) return -1;
    snprintf(buf, cap, "%s", e);
    return 0;
}
int sys_registry_get_int(uint32_t key, int *out, uint32_t *err) {
    (void)key, (void)out, (void)err;
    return -1;
}
int sys_registry_set_int(uint32_t key, int val, uint32_t *err) {
    (void)key, (void)val, (void)err;
    return -1;
}
int sys_registry_get_bin(uint32_t key, void *buf, size_t cap, uint32_t *err) {
    (void)key, (void)buf, (void)cap, (void)err;
    return -1;
}
int sys_registry_set_bin(uint32_t key, const void *buf, size_t cap, uint32_t *err) {
    (void)key, (void)buf, (void)cap, (void)err;
    return -1;
}

static int padded(const char *field, const char *want) {
    char expect[ENTITY_SIZE] = {0};
    memcpy(expect, want, strlen(want));
    return memcmp(field, expect, ENTITY_SIZE) == 0;
}

int main(void) {
    uint32_t err = 0xFFFFFFFFu;
    char back[PROFILE_NAME_MAX];

    CHECK(PROFILE_NAME_FIELD == ENTITY_SIZE);
    CHECK(PROFILE_NAME_BYTES_MAX == 16);

    /* The rename of #940: the console takes it, because the size is the entity's. */
    CHECK(profile_slot_set_name(1, "david", &err) == 0);
    CHECK(err == 0);
    CHECK(g_last_len == ENTITY_SIZE);
    CHECK(padded(g_name[1], "david"));
    CHECK(profile_slot_get_name(1, back, NULL) == 0 && strcmp(back, "david") == 0);

    /* A shorter name over a longer one leaves none of the old one behind. */
    CHECK(profile_slot_set_name(1, "Al", &err) == 0);
    CHECK(padded(g_name[1], "Al"));

    /* Exactly 16 bytes fits, in the last slot too. */
    CHECK(profile_slot_set_name(PROFILE_SLOT_COUNT, "0123456789abcdef", &err) == 0);
    CHECK(padded(g_name[PROFILE_SLOT_COUNT], "0123456789abcdef"));

    /* 17 bytes does not: refused with our own code, nothing sent, the old name kept. */
    int before = g_writes;
    err = 0;
    CHECK(profile_slot_set_name(1, "0123456789abcdefg", &err) == -1);
    CHECK(err == SYS_REGISTRY_ERR_VALUE_TOO_LONG);
    CHECK(g_writes == before);
    CHECK(padded(g_name[1], "Al"));

    /* Bytes, not letters: eight 2-byte letters fit, six 3-byte ones (18 bytes) do not, and the
     * refused one is not cut in the middle of a letter. */
    CHECK(profile_slot_set_name(2, "\xC3\xA9\xC3\xA9\xC3\xA9\xC3\xA9\xC3\xA9\xC3\xA9\xC3\xA9\xC3\xA9",
                                &err) == 0);
    CHECK(strlen(g_name[2]) == 16);
    err = 0;
    CHECK(profile_slot_set_name(
              2, "\xE6\x97\xA5\xE6\x9C\xAC\xE8\xAA\x9E\xE6\x97\xA5\xE6\x9C\xAC\xE8\xAA\x9E", &err) == -1);
    CHECK(err == SYS_REGISTRY_ERR_VALUE_TOO_LONG);
    CHECK(strlen(g_name[2]) == 16);

    /* No name is no write. */
    before = g_writes;
    err = 0;
    CHECK(profile_slot_set_name(1, "", &err) == -1 && err == SYS_REGISTRY_ERR_NULL_ARG);
    err = 0;
    CHECK(profile_slot_set_name(1, NULL, &err) == -1 && err == SYS_REGISTRY_ERR_NULL_ARG);
    CHECK(profile_slot_set_name(1, "x", NULL) == 0); /* a caller that wants no code */
    CHECK(g_writes == before + 1);

    /* The type entity is 17 as well, and is written with it. */
    CHECK(profile_slot_set_type(3, "np", &err) == 0);
    CHECK(g_last_len == ENTITY_SIZE && padded(g_type[3], "np"));

    /* The local user name has the same room; too long never reaches Sony. */
    CHECK(profile_set_local_username(0x10000001u, "0123456789abcdefg") == -1);
    CHECK(profile_set_local_username(0x10000001u, "") == -1);

    if (failures) {
        fprintf(stderr, "%d failure(s)\n", failures);
        return 1;
    }
    printf("profile name selftest: ok\n");
    return 0;
}
