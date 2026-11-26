/* Host-side tests for cheats_plan.c: parsing a cheat's locations, telling which state each is
 * in, and the order the writes go in. The data is the real Marvel's Wolverine (PPSA03671,
 * 01.001.005) cheat pack the app downloads, whose cheats crashed the console with the old
 * engine (fix/cheats-wolverine). */
#include <stdio.h>
#include <string.h>

#include "../src/cheats_plan.c"

static int failures = 0;

#define CHECK(expr)                                                 \
    do {                                                            \
        if (!(expr)) {                                              \
            fprintf(stderr, "FAIL line %d: %s\n", __LINE__, #expr); \
            failures++;                                             \
        }                                                           \
    } while (0)

typedef struct { const char *off, *on, *orig; } raw_t;

/* "Max Resources / XP" from PPSA03671_01.001.005_50d8b46b.json, in file order: 16 code caves
 * (original bytes are int3 padding) and 4 hooks that jump into them. */
static const raw_t MAX_RES[] = {
    {"05CC0A91", "4C8B184D8B5B30E9E4060000", "CCCCCCCCCCCCCCCCCCCCCCCC"},
    {"05CC1181", "4D85DB0F84D72D0000E932060000", "CCCCCCCCCCCCCCCCCCCCCCCCCCCC"},
    {"05CC17C1", "450FB65370E9F6050000", "CCCCCCCCCCCCCCCCCCCC"},
    {"05CC1DC1", "4584D20F8497210000E942010000", "CCCCCCCCCCCCCCCCCCCCCCCCCCCC"},
    {"05CC1F11", "837808000F8E46200000E9111E0000", "CCCCCCCCCCCCCCCCCCCCCCCCCCCCCC"},
    {"05CC3D31", "C740089F860100C3", "CCCCCCCCCCCCCCCC"},
    {"05CC3F61", "C3", "CC"},
    {"05CC4091", "E8FBC9FFFF8B4808E97D7F0100", "CCCCCCCCCCCCCCCCCCCCCCCCCC"},
    {"05CDC014", "E97880FEFF", "8B4808EB02"},
    {"05CC5041", "E84BBAFFFF8B4808E99D700100", "CCCCCCCCCCCCCCCCCCCCCCCCCC"},
    {"05CDC0E4", "E9588FFEFF", "8B4808EB02"},
    {"05CC50A1", "E8EBB9FFFF8B7008E91E710100", "CCCCCCCCCCCCCCCCCCCCCCCCCC"},
    {"05CDC1C5", "E9D78EFEFF", "8B7008EB02"},
    {"05CC7A81", "4C8B5B30E907000000", "CCCCCCCCCCCCCCCCCC"},
    {"05CC7A91", "4D85DB0F84570F0000E922020000", "CCCCCCCCCCCCCCCCCCCCCCCCCCCC"},
    {"05CC7CC1", "450FB65370E926030000", "CCCCCCCCCCCCCCCCCCCC"},
    {"05CC7FF1", "4584D20F84F7090000E952000000", "CCCCCCCCCCCCCCCCCCCCCCCCCCCC"},
    {"05CC8051", "41BD9F860100E995090000", "CCCCCCCCCCCCCCCCCCCCCC"},
    {"05CC89F1", "4489EB45896F08E9F73F0100", "CCCCCCCCCCCCCCCCCCCCCCCC"},
    {"05CDC9ED", "E98FB0FEFF9090", "4489EB45896F08"},
};
#define N_MAX_RES ((int)(sizeof MAX_RES / sizeof MAX_RES[0]))

/* "Infinite Health" from PPSA03671_01.001.005_d02e93c8.mc4: the hook comes SECOND in the file,
 * the cave is zero padding at the end of .text, and the values use dash separators. */
static const raw_t MC4_HEALTH[] = {
    {"4803568", "E9-D3-04-72-01-90-90-90", "C5-FA-11-83-D0-00-00-00"},
    {"5F23A40",
     "80-BB-4C-01-00-00-00-75-0F-C7-83-D0-00-00-00-00-00-96-43-E9-18-FB-8D-FE-C5-FA-11-83-D0-00-00-00-E9-0B-FB-8D-FE",
     "00-00-00-00-00-00-00-00-00-00-00-00-00-00-00-00-00-00-00-00-00-00-00-00-00-00-00-00-00-00-00-00-00-00-00-00-00"},
};

static int build(const raw_t *raw, int n, intptr_t base, cheat_loc_t *locs) {
    for (int i = 0; i < n; i++) {
        uint64_t off = 0;
        if (cheat_parse_offset(raw[i].off, &off) != 0) return -1;
        int a = cheat_parse_hex(raw[i].on, locs[i].on, CHEAT_PLAN_MAX_BYTES);
        int b = cheat_parse_hex(raw[i].orig, locs[i].off, CHEAT_PLAN_MAX_BYTES);
        if (a <= 0 || a != b) return -1;
        locs[i].len = a;
        locs[i].addr = base + (intptr_t)off;
        memcpy(locs[i].cur, locs[i].off, (size_t)a); /* the game as it boots */
    }
    return 0;
}

/* Position of location `idx` in a write order. */
static int pos_of(const int *order, int n, int idx) {
    for (int k = 0; k < n; k++)
        if (order[k] == idx) return k;
    return -1;
}

static void test_parse(void) {
    uint8_t b[8];
    CHECK(cheat_parse_hex("90 1F-00,00", b, 8) == 4 && b[0] == 0x90 && b[1] == 0x1F && b[3] == 0);
    CHECK(cheat_parse_hex("C3", b, 8) == 1 && b[0] == 0xC3);
    CHECK(cheat_parse_hex("ABC", b, 8) == 2 && b[0] == 0xAB && b[1] == 0x0C);
    CHECK(cheat_parse_hex("", b, 8) == -1);
    CHECK(cheat_parse_hex("9G", b, 8) == -1);
    CHECK(cheat_parse_hex("0102030405", b, 4) == -1); /* longer than the buffer: refused, not cut */

    uint64_t o = 0;
    CHECK(cheat_parse_offset("01BAD908", &o) == 0 && o == 0x1BAD908);
    CHECK(cheat_parse_offset("0x1bad908", &o) == 0 && o == 0x1BAD908);
    CHECK(cheat_parse_offset(" 5F23A40\n", &o) == 0 && o == 0x5F23A40);
    CHECK(cheat_parse_offset("5C0-executable[0]-5", &o) == -1); /* MC4 <PS5Offset> form */
    CHECK(cheat_parse_offset("", &o) == -1);
}

static void test_state(void) {
    cheat_loc_t l[2];
    /* God Mode, the JSON pack: base 0x400000 measured on the Pro (FW 13.60). */
    static const raw_t god[] = {
        {"01BAD908", "909090909090909090", "C4E2719D8FF0010000"},
        {"01BAD911", "C5F828C1", "C5F25DC0"},
    };
    CHECK(build(god, 2, 0x400000, l) == 0);
    CHECK(l[0].addr == 0x1FAD908 && l[0].len == 9 && l[1].len == 4);
    CHECK(cheat_loc_state(&l[0]) == CHEAT_LOC_OFF);
    memcpy(l[0].cur, l[0].on, 9);
    CHECK(cheat_loc_state(&l[0]) == CHEAT_LOC_ON);
    l[0].cur[3] ^= 0xFF; /* another build, or another cheat's jump already there */
    CHECK(cheat_loc_state(&l[0]) == CHEAT_LOC_MISMATCH);
    CHECK(!cheat_loc_is_cave(&l[0]) && !cheat_loc_is_cave(&l[1]));
}

static void test_order_max_resources(void) {
    cheat_loc_t locs[N_MAX_RES];
    CHECK(build(MAX_RES, N_MAX_RES, 0x400000, locs) == 0);
    int hooks[] = {8, 10, 12, 19};
    int caves = 0;
    for (int i = 0; i < N_MAX_RES; i++) caves += cheat_loc_is_cave(&locs[i]);
    CHECK(caves == 16);
    for (int h = 0; h < 4; h++) CHECK(!cheat_loc_is_cave(&locs[hooks[h]]));

    int on[N_MAX_RES], off[N_MAX_RES];
    cheat_plan_order(locs, N_MAX_RES, 1, on);
    cheat_plan_order(locs, N_MAX_RES, 0, off);
    /* Every location exactly once. */
    int seen[N_MAX_RES] = {0};
    for (int k = 0; k < N_MAX_RES; k++) seen[on[k]]++;
    for (int i = 0; i < N_MAX_RES; i++) CHECK(seen[i] == 1);
    /* Turning on: every cave is written before any hook. In file order, the hook at
     * 05CDC014 (index 8) came before the cave at 05CC5041 (index 9). */
    for (int h = 0; h < 4; h++)
        for (int i = 0; i < N_MAX_RES; i++)
            if (cheat_loc_is_cave(&locs[i])) CHECK(pos_of(on, N_MAX_RES, i) < pos_of(on, N_MAX_RES, hooks[h]));
    /* Turning off: every hook is restored before any cave goes back to int3. In file order
     * the cave at 05CC4091 (index 7) was cleared while the hook into it (index 8) was live. */
    for (int h = 0; h < 4; h++)
        for (int i = 0; i < N_MAX_RES; i++)
            if (cheat_loc_is_cave(&locs[i])) CHECK(pos_of(off, N_MAX_RES, hooks[h]) < pos_of(off, N_MAX_RES, i));
    for (int k = 0; k < N_MAX_RES; k++) CHECK(off[k] == on[N_MAX_RES - 1 - k]);
}

static void test_order_mc4_zero_cave(void) {
    cheat_loc_t locs[2];
    CHECK(build(MC4_HEALTH, 2, 0x400000, locs) == 0);
    CHECK(locs[0].len == 8 && locs[1].len == 37);
    CHECK(!cheat_loc_is_cave(&locs[0]) && cheat_loc_is_cave(&locs[1]));
    int on[2], off[2];
    cheat_plan_order(locs, 2, 1, on);
    cheat_plan_order(locs, 2, 0, off);
    CHECK(on[0] == 1 && on[1] == 0);   /* cave, then the jump into it */
    CHECK(off[0] == 0 && off[1] == 1); /* jump restored, then the cave cleared */
    /* The cave ends inside .text: 0x400000 + 0x5F23A40 + 37 < 0x6324000 (measured end). */
    CHECK(locs[1].addr + locs[1].len <= 0x6324000);
}

static void test_module_is_game(void) {
    CHECK(cheat_module_is_game(0, "/app0/eboot.bin"));
    CHECK(cheat_module_is_game(0, "/system_ex/app/PPSA01234/eboot.bin"));
    CHECK(cheat_module_is_game(0, "/mnt/sandbox/pfsmnt/CUSA00284-app0/EBOOT.BIN"));
    CHECK(cheat_module_is_game(0, "")); /* path unreadable: as handle 0 always was */
    CHECK(cheat_module_is_game(0, NULL));
    CHECK(cheat_module_is_game(7, "/app0/sce_module/libgame.prx"));
    /* #920: handle 0 of a PS4 game's process is another module; a physical write there is
     * shared with every process mapping it. */
    CHECK(!cheat_module_is_game(0, "/system/common/lib/libSceSomething.sprx"));
    CHECK(!cheat_module_is_game(0, "/system_ex/common_ex/lib/libc.prx"));
    CHECK(!cheat_module_is_game(3, "/system/common/lib/libkernel.sprx"));
    CHECK(!cheat_module_is_game(3, ""));
    /* #946: a PS4 game's eboot.bin under another handle, from its sandbox path. */
    CHECK(cheat_module_is_game(5, "/mnt/sandbox/pfsmnt/CUSA00284-app0/eboot.bin"));
    CHECK(cheat_module_is_game(5, "/mnt/sandbox/CUSA00284_000/app0/eboot.bin"));
    /* A system app's eboot.bin is not the game's. */
    CHECK(!cheat_module_is_game(5, "/system/vsh/app/NPXS20001/eboot.bin"));
    CHECK(!cheat_module_is_game(5, "/system_ex/app/NPXS40087/eboot.bin"));
    CHECK(!cheat_module_is_game(5, "/preinst/app/eboot.bin"));
    CHECK(!cheat_module_is_game(5, "/mnt/sandbox/pfsmnt/CUSA00284-app0/libfoo.prx"));
}

static cheat_exe_seen_t seen(int found, uint64_t base, uint64_t size, const char *path) {
    cheat_exe_seen_t s;
    memset(&s, 0, sizeof s);
    s.found = found;
    s.base = base;
    s.size = size;
    snprintf(s.path, sizeof s.path, "%s", path);
    return s;
}

/* #946: where cheat offsets are relative to. The toggle said "the game's code is not loaded yet"
 * (base 0) for Rayman Legends (CUSA00284, PS4) on a PS5 while a debugger saw its code at
 * 0x400000. Every case a PS4 game can be in gives a base; a PS5 game gives one only from its
 * handle 0. */
static void test_exe_base_pick(void) {
    int how = -1;
    /* PS5: handle 0, whatever it is (the write check judges the module). */
    cheat_exe_seen_t ps5 = seen(1, 0x7ff00000000ULL, 0x6000000, "/app0/eboot.bin");
    CHECK(cheat_exe_base_pick(0, &ps5, NULL, &how) == 0x7ff00000000ULL && how == CHEAT_EXE_HANDLE0);
    cheat_exe_seen_t none = seen(0, 0, 0, "");
    CHECK(cheat_exe_base_pick(0, &none, NULL, &how) == 0 && how == CHEAT_EXE_NONE);
    cheat_exe_seen_t zero = seen(1, 0, 0x6000000, "/app0/eboot.bin");
    CHECK(cheat_exe_base_pick(0, &zero, NULL, &how) == 0 && how == CHEAT_EXE_NONE);
    CHECK(cheat_exe_base_pick(0, NULL, NULL, NULL) == 0);

    /* PS4, handle 0 is its eboot.bin at a usable address: that address. */
    cheat_exe_seen_t ps4 = seen(1, 0x400000, 0x1600000, "/mnt/sandbox/pfsmnt/CUSA00284-app0/eboot.bin");
    CHECK(cheat_exe_base_pick(1, &ps4, NULL, &how) == 0x400000 && how == CHEAT_EXE_HANDLE0);
    /* PS4, handle 0 named eboot.bin but listed at 0 (12.6.26 returned 0 here: "not loaded"). */
    cheat_exe_seen_t ps4_zero = seen(1, 0, 0x1600000, "/mnt/sandbox/pfsmnt/CUSA00284-app0/eboot.bin");
    CHECK(cheat_exe_base_pick(1, &ps4_zero, NULL, &how) == CHEAT_PS4_EXE_BASE &&
          how == CHEAT_EXE_PS4_DEFAULT);
    /* PS4, handle 0 not in the list at all (12.6.26: 0 unless one read at 0x400000 worked). */
    CHECK(cheat_exe_base_pick(1, &none, NULL, &how) == CHEAT_PS4_EXE_BASE &&
          how == CHEAT_EXE_PS4_DEFAULT);
    CHECK(cheat_exe_base_pick(1, NULL, NULL, &how) == CHEAT_PS4_EXE_BASE);
    /* PS4, handle 0 is another module. */
    cheat_exe_seen_t other = seen(1, 0x6988c000ULL, 0x54000, "/system/common/lib/libSceSomething.sprx");
    CHECK(cheat_exe_base_pick(1, &other, NULL, &how) == CHEAT_PS4_EXE_BASE &&
          how == CHEAT_EXE_PS4_DEFAULT);
    /* ... with the game's eboot.bin under another handle: that one. */
    cheat_exe_seen_t named = seen(1, 0x800000, 0x1600000, "/mnt/sandbox/pfsmnt/CUSA00284-app0/eboot.bin");
    CHECK(cheat_exe_base_pick(1, &other, &named, &how) == 0x800000 && how == CHEAT_EXE_NAMED);
    /* ... but not a named one at 0, or a system app's. */
    cheat_exe_seen_t named_zero = seen(1, 0, 0x1600000, "/mnt/sandbox/pfsmnt/CUSA00284-app0/eboot.bin");
    CHECK(cheat_exe_base_pick(1, &other, &named_zero, &how) == CHEAT_PS4_EXE_BASE);
    cheat_exe_seen_t named_sys = seen(1, 0x900000, 0x1000, "/system/vsh/app/NPXS20001/eboot.bin");
    CHECK(cheat_exe_base_pick(1, &other, &named_sys, &how) == CHEAT_PS4_EXE_BASE);
    /* A handle 0 at a kernel address is no base either. */
    cheat_exe_seen_t kern = seen(1, 0xFFFFF04080F57368ULL, 0x1000, "/app0/eboot.bin");
    CHECK(cheat_exe_base_pick(0, &kern, NULL, &how) == 0 && how == CHEAT_EXE_NONE);
    CHECK(cheat_exe_base_pick(1, &kern, NULL, &how) == CHEAT_PS4_EXE_BASE);

    /* The Rayman Legends "Infinite Live" location (CUSA00031/CUSA00284 01.00, offset F3A54)
     * lands where the debugger read its original bytes 29 F0. */
    uint64_t off = 0;
    CHECK(cheat_parse_offset("F3A54", &off) == 0);
    CHECK(cheat_exe_base_pick(1, &ps4_zero, NULL, NULL) + off == 0x4F3A54);
}

static void test_exe_describe(void) {
    char out[160];
    cheat_exe_seen_t none = seen(0, 0, 0, "");
    cheat_exe_describe(&none, out, sizeof out);
    CHECK(strcmp(out, "not in the module list") == 0);
    cheat_exe_describe(NULL, out, sizeof out);
    CHECK(strcmp(out, "not in the module list") == 0);
    cheat_exe_seen_t e = seen(1, 0, 0x1600000, "/mnt/sandbox/pfsmnt/CUSA00284-app0/eboot.bin");
    cheat_exe_describe(&e, out, sizeof out);
    /* The file name only, never the whole path. */
    CHECK(strcmp(out, "eboot.bin at 0x0, 0x1600000 bytes") == 0);
    cheat_exe_seen_t np = seen(1, 0x400000, 0x10, "");
    cheat_exe_describe(&np, out, sizeof out);
    CHECK(strcmp(out, "(no path) at 0x400000, 0x10 bytes") == 0);
    char tiny[6];
    memset(tiny, 'x', sizeof tiny);
    cheat_exe_describe(&e, tiny, sizeof tiny);
    CHECK(tiny[sizeof tiny - 1] == '\0');
}

/* An XML game patch's base module goes by the same rule (it used to take handle 0 by number). */
static void test_patch_base_ok(void) {
    char why[320];
    CHECK(cheat_patch_base_ok(0, "/app0/eboot.bin", why, sizeof why) && why[0] == '\0');
    CHECK(cheat_patch_base_ok(0, "/mnt/sandbox/pfsmnt/CUSA00284-app0/eboot.bin", why, sizeof why));
    CHECK(cheat_patch_base_ok(0, "", why, sizeof why)); /* unreadable path: as handle 0 always was */
    CHECK(cheat_patch_base_ok(5, "/app0/sce_module/libgame.prx", NULL, 0));
    /* #920: a PS4 game's handle 0 is another module (Rayman Legends, CUSA00284 on 11.60). */
    CHECK(!cheat_patch_base_ok(0, "/system/common/lib/libScePlayGo.sprx", why, sizeof why));
    CHECK(strstr(why, "libScePlayGo.sprx") != NULL && strstr(why, "nothing was written") != NULL);
    /* A .prx AppElf that resolves to a system library. */
    CHECK(!cheat_patch_base_ok(9, "/system/common/lib/libSceGnmDriver.sprx", why, sizeof why));
    CHECK(strstr(why, "is not one of the game's own modules") != NULL);
    CHECK(!cheat_patch_base_ok(9, "/system/common/lib/libSceGnmDriver.sprx", NULL, 0));
    /* A small buffer is cut, never overrun. */
    {
        char tiny[8];
        memset(tiny, 'x', sizeof tiny);
        CHECK(!cheat_patch_base_ok(0, "/system/common/lib/libc.sprx", tiny, sizeof tiny));
        CHECK(tiny[sizeof tiny - 1] == '\0');
    }
}

static void test_addr_ok(void) {
    CHECK(cheat_addr_ok(0x1FAD908, 9));
    CHECK(!cheat_addr_ok(0, 4));
    CHECK(!cheat_addr_ok(0x1BAD908 - 0x1BAD908 + 0x100, 4)); /* null page */
    CHECK(!cheat_addr_ok((intptr_t)0xFFFFF04080F57368ULL, 4)); /* kernel address */
    CHECK(!cheat_addr_ok(0x7FFFFFFFFFFE, 4));                   /* crosses the user limit */
    CHECK(!cheat_addr_ok(0x400000, 0));
}


/* ── A fake game memory for cheat_plan_apply ───────────────────────── */

#define MOCK_BASE 0x400000
#define MOCK_SIZE 0x8000
#define MOCK_PAGE 0x1000

typedef struct {
    uint8_t mem[MOCK_SIZE];
    int calls;        /* write calls so far */
    int fail_call;    /* this call (1-based) fails; 0 = never */
    int fail_from;    /* this call and every later one fails; 0 = never */
    int split_fail;   /* the failing call writes its first page, then fails */
    int alive_until;  /* alive() turns 0 after this many write calls; 0 = always alive */
} mock_t;

static int mock_write(void *opaque, intptr_t addr, const uint8_t *data, size_t len) {
    mock_t *m = (mock_t *)opaque;
    m->calls++;
    int fail = (m->fail_call && m->calls == m->fail_call) || (m->fail_from && m->calls >= m->fail_from);
    size_t off = (size_t)(addr - MOCK_BASE);
    if (off + len > MOCK_SIZE) return -1;
    size_t done = 0;
    while (done < len) { /* page by page, like the physical-page path */
        size_t page_end = ((off + done) / MOCK_PAGE + 1) * MOCK_PAGE;
        size_t chunk = page_end - (off + done);
        if (chunk > len - done) chunk = len - done;
        if (fail && (!m->split_fail || done > 0)) return -1;
        memcpy(m->mem + off + done, data + done, chunk);
        done += chunk;
    }
    return fail ? -1 : 0;
}

static int mock_alive(void *opaque) {
    mock_t *m = (mock_t *)opaque;
    return !m->alive_until || m->calls < m->alive_until;
}

/* A synthetic hook-and-cave cheat in the fake memory: 3 caves (int3 padding), 2 hooks, and one
 * cave that straddles the page boundary at 0x402000. */
static const raw_t SYN[] = {
    {"0100", "4C8B184D8B5B30E9E4060000", "CCCCCCCCCCCCCCCCCCCCCCCC"},
    {"5000", "E97880FEFF", "8B4808EB02"},
    {"1FF8", "450FB65370E9F6050000AABBCCDD", "CCCCCCCCCCCCCCCCCCCCCCCCCCCC"}, /* crosses 0x2000 */
    {"5100", "E9588FFEFF", "8B4808EB02"},
    {"0200", "C3", "CC"},
};
#define N_SYN ((int)(sizeof SYN / sizeof SYN[0]))

static void mock_boot(mock_t *m, cheat_loc_t *locs) {
    memset(m, 0, sizeof *m);
    memset(m->mem, 0x90, sizeof m->mem);
    build(SYN, N_SYN, MOCK_BASE, locs);
    for (int i = 0; i < N_SYN; i++)
        memcpy(m->mem + (locs[i].addr - MOCK_BASE), locs[i].off, (size_t)locs[i].len);
}

static void mock_read_cur(const mock_t *m, cheat_loc_t *locs, int n) {
    for (int i = 0; i < n; i++)
        memcpy(locs[i].cur, m->mem + (locs[i].addr - MOCK_BASE), (size_t)locs[i].len);
}

static int all_in_state(const mock_t *m, const cheat_loc_t *locs, int n, int on) {
    for (int i = 0; i < n; i++)
        if (memcmp(m->mem + (locs[i].addr - MOCK_BASE), on ? locs[i].on : locs[i].off,
                   (size_t)locs[i].len) != 0)
            return 0;
    return 1;
}

static void test_apply_mock(void) {
    static mock_t m, snap;
    cheat_loc_t locs[N_SYN];
    cheat_mem_ops_t ops = {&m, mock_write, mock_alive};
    char err[256];

    /* On, then off. */
    mock_boot(&m, locs);
    mock_read_cur(&m, locs, N_SYN);
    CHECK(cheat_plan_apply(locs, N_SYN, 1, NULL, 0, &ops, err, sizeof err) == 0);
    CHECK(all_in_state(&m, locs, N_SYN, 1));
    mock_read_cur(&m, locs, N_SYN);
    CHECK(cheat_plan_apply(locs, N_SYN, 0, NULL, 0, &ops, err, sizeof err) == 0);
    CHECK(all_in_state(&m, locs, N_SYN, 0));

    /* The Nth write fails, for every N: everything is put back, including what was half done. */
    for (int nth = 1; nth <= N_SYN; nth++) {
        for (int split = 0; split <= 1; split++) {
            mock_boot(&m, locs);
            snap = m;
            m.fail_call = nth;
            m.split_fail = split;
            mock_read_cur(&m, locs, N_SYN);
            CHECK(cheat_plan_apply(locs, N_SYN, 1, NULL, 0, &ops, err, sizeof err) != 0);
            CHECK(memcmp(m.mem, snap.mem, MOCK_SIZE) == 0);
            CHECK(strstr(err, "left as it was") != NULL);
        }
    }

    /* The page-straddling cave fails on its second page: torn, then restored. Caves go first in
     * file order, so it is the 2nd write. */
    mock_boot(&m, locs);
    snap = m;
    m.fail_call = 2;
    m.split_fail = 1;
    mock_read_cur(&m, locs, N_SYN);
    CHECK(cheat_plan_apply(locs, N_SYN, 1, NULL, 0, &ops, err, sizeof err) != 0);
    CHECK(memcmp(m.mem, snap.mem, MOCK_SIZE) == 0);

    /* Same while turning off. */
    mock_boot(&m, locs);
    mock_read_cur(&m, locs, N_SYN);
    CHECK(cheat_plan_apply(locs, N_SYN, 1, NULL, 0, &ops, err, sizeof err) == 0);
    snap = m;
    m.calls = 0;
    m.fail_call = 4;
    m.split_fail = 1;
    mock_read_cur(&m, locs, N_SYN);
    CHECK(cheat_plan_apply(locs, N_SYN, 0, NULL, 0, &ops, err, sizeof err) != 0);
    CHECK(memcmp(m.mem, snap.mem, MOCK_SIZE) == 0);

    /* The undo itself fails: said so, not "left as it was". */
    mock_boot(&m, locs);
    m.fail_from = 3;
    mock_read_cur(&m, locs, N_SYN);
    CHECK(cheat_plan_apply(locs, N_SYN, 1, NULL, 0, &ops, err, sizeof err) != 0);
    CHECK(strstr(err, "could not be put back") != NULL);

    /* A mismatch anywhere: nothing written at all. */
    mock_boot(&m, locs);
    m.mem[locs[3].addr - MOCK_BASE] = 0x00;
    snap = m;
    mock_read_cur(&m, locs, N_SYN);
    CHECK(cheat_plan_apply(locs, N_SYN, 1, NULL, 0, &ops, err, sizeof err) != 0);
    CHECK(m.calls == 0 && memcmp(m.mem, snap.mem, MOCK_SIZE) == 0);

    /* The game exits mid-apply: stops writing. */
    mock_boot(&m, locs);
    m.alive_until = 2;
    mock_read_cur(&m, locs, N_SYN);
    CHECK(cheat_plan_apply(locs, N_SYN, 1, NULL, 0, &ops, err, sizeof err) != 0);
    CHECK(m.calls == 2);
    CHECK(strstr(err, "no game") != NULL);

    /* Turning off with another enabled cheat sharing the first cave: that cave keeps its code,
     * everything else goes back. Even when the other cheat's bytes differ from ours there. */
    mock_boot(&m, locs);
    mock_read_cur(&m, locs, N_SYN);
    CHECK(cheat_plan_apply(locs, N_SYN, 1, NULL, 0, &ops, err, sizeof err) == 0);
    m.mem[locs[0].addr - MOCK_BASE + 2] = 0x77; /* the other cheat's version of the cave */
    uint8_t cave_before[32];
    memcpy(cave_before, m.mem + (locs[0].addr - MOCK_BASE), (size_t)locs[0].len);
    /* The second range overlaps the hook at locs[1]: only caves are shared, so the hook must
     * still be restored. */
    cheat_range_t keep[2] = {{locs[0].addr + 4, 2}, {locs[1].addr + 1, 2}};
    CHECK(keep[1].addr < locs[1].addr + locs[1].len && locs[1].addr < keep[1].addr + keep[1].len);
    mock_read_cur(&m, locs, N_SYN);
    CHECK(cheat_plan_apply(locs, N_SYN, 0, keep, 2, &ops, err, sizeof err) == 0);
    CHECK(memcmp(m.mem + (locs[0].addr - MOCK_BASE), cave_before, (size_t)locs[0].len) == 0);
    CHECK(locs[0].keep == 1);
    CHECK(locs[1].keep == 0);
    CHECK(memcmp(m.mem + (locs[1].addr - MOCK_BASE), locs[1].off, (size_t)locs[1].len) == 0);
    CHECK(all_in_state(&m, locs + 1, N_SYN - 1, 0));
}

/* An XML game patch (#527) is one "cheat" with hundreds of locations: past the 32 the JSON/SHN
 * formats hold, still all or nothing, and a failure late in the run puts every earlier one back. */
static void test_apply_many(void) {
    static mock_t m;
    enum { N = 300 };
    static cheat_loc_t locs[N];
    cheat_mem_ops_t ops = {&m, mock_write, mock_alive};
    char err[256];
    memset(&m, 0, sizeof m);
    memset(m.mem, 0x90, sizeof m.mem);
    memset(locs, 0, sizeof locs);
    for (int i = 0; i < N; i++) {
        locs[i].addr = MOCK_BASE + 0x10 * i;
        locs[i].len = 4;
        memset(locs[i].on, 0xA0 + (i & 0xF), 4);
        memset(locs[i].off, 0x90, 4);
    }
    mock_read_cur(&m, locs, N);
    CHECK(cheat_plan_apply(locs, N, 1, NULL, 0, &ops, err, sizeof err) == 0);
    CHECK(all_in_state(&m, locs, N, 1));
    mock_read_cur(&m, locs, N);
    CHECK(cheat_plan_apply(locs, N, 0, NULL, 0, &ops, err, sizeof err) == 0);
    CHECK(all_in_state(&m, locs, N, 0));

    m.calls = 0;
    m.fail_call = 250;
    mock_read_cur(&m, locs, N);
    CHECK(cheat_plan_apply(locs, N, 1, NULL, 0, &ops, err, sizeof err) == -1);
    CHECK(all_in_state(&m, locs, N, 0));
    CHECK(strstr(err, "left as it was") != NULL);
    m.fail_call = 0;
    CHECK(cheat_plan_apply(locs, CHEAT_PLAN_MAX_APPLY + 1, 1, NULL, 0, &ops, err, sizeof err) == -1);
}

int main(void) {
    test_apply_many();
    test_parse();
    test_state();
    test_order_max_resources();
    test_order_mc4_zero_cave();
    test_addr_ok();
    test_module_is_game();
    test_patch_base_ok();
    test_exe_base_pick();
    test_exe_describe();
    test_apply_mock();
    if (failures) {
        fprintf(stderr, "cheats_plan_selftest: %d failure(s)\n", failures);
        return 1;
    }
    printf("cheats_plan_selftest: ok\n");
    return 0;
}
