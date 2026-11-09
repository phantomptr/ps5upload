/* Host-side tests for the multi-device mesh (mesh.c): who gets in (open, approval, blocked), the
 * device list (hello, the same device under a new key, rename/remove/block, prune, reload from disk),
 * presence, locks (held, forced, lapsing when the holder leaves), events, settings and the shared
 * documents (revisions, a stale write answered with the current document). Files go to a fresh
 * mkdtemp directory. Build: see the root Makefile test-payload. */
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

#include "mesh.h"

static int failures = 0;

#define CHECK(expr)                                                 \
    do {                                                            \
        if (!(expr)) {                                              \
            fprintf(stderr, "FAIL line %d: %s\n", __LINE__, #expr); \
            failures++;                                             \
        }                                                           \
    } while (0)

static char g_dir[256];
static uint64_t g_now = 1700000000000ull;
static mesh_online_t g_on[8];
static int g_non;
static uint8_t g_forgot[16][32];
static int g_nforgot;

static int hook_online(mesh_online_t *out, int max) {
    int i;
    for (i = 0; i < g_non && i < max; i++) out[i] = g_on[i];
    return i;
}
static int hook_forget(const uint8_t key[32]) {
    if (g_nforgot < 16) memcpy(g_forgot[g_nforgot++], key, 32);
    return 0;
}
static uint64_t hook_now(void) { return g_now; }

static uint64_t g_mono = 5000;
static uint64_t hook_mono(void) { return g_mono; }
static const mesh_hooks_t HOOKS = { hook_online, hook_forget, hook_now, hook_mono };

static void key_of(int n, uint8_t k[32]) {
    memset(k, 0, 32);
    k[0] = (uint8_t)n;
    k[31] = 0xAB;
}

static void online(int n, uint32_t ip) {
    key_of(n, g_on[g_non].key);
    g_on[g_non].ip = ip;
    g_non++;
}

static char g_out[MESH_REPLY_MAX];

static const char *call(int which, int who, const char *body) {
    uint8_t k[32];
    int n;
    key_of(who, k);
    n = mesh_call(which, k, body, strlen(body), g_out, sizeof g_out);
    CHECK(n >= 0);
    return n >= 0 ? g_out : "";
}

static int has(const char *hay, const char *needle) { return strstr(hay, needle) != NULL; }

/* The module reloaded from what is on disk. */
static void reload(void) {
    g_non = 0;
    g_nforgot = 0;
    CHECK(mesh_start(g_dir, &HOOKS, "8.0.0") == 0);
}

/* A console that never had a device. */
static void start(void) {
    char cmd[400];
    snprintf(cmd, sizeof cmd, "rm -rf '%s/v1'", g_dir);
    if (system(cmd) != 0) fprintf(stderr, "could not clear %s\n", g_dir);
    reload();
}

static void test_admission(void) {
    uint8_t a[32], b[32];
    key_of(1, a);
    key_of(2, b);
    start();
    CHECK(mesh_admit(a, "MacBook", 0x0100a8c0u) == 1); /* open by default */
    CHECK(mesh_refused(a) == 0);
    call(MESH_SETTINGS, 1, "{\"require_approval\":true}");
    CHECK(mesh_admit(b, "Pixel", 0x0200a8c0u) == 0); /* the pairing rules decide */
    call(MESH_SETTINGS, 1, "{\"require_approval\":false}");
    CHECK(mesh_admit(b, "Pixel", 0x0200a8c0u) == 1);
    online(1, 0x0100a8c0u);
    CHECK(has(call(MESH_DEVICE, 1, "{\"op\":\"block\",\"id\":\"02000000000000000000000000000000000000000000000000000000000000ab\"}"),
              "\"ok\":true"));
    CHECK(g_nforgot == 1 && memcmp(g_forgot[0], b, 32) == 0); /* blocking ends its sessions */
    CHECK(mesh_refused(b) == 1);
    CHECK(mesh_admit(b, "Pixel", 0x0200a8c0u) == -1);
    call(MESH_DEVICE, 1, "{\"op\":\"unblock\",\"id\":\"02000000000000000000000000000000000000000000000000000000000000ab\"}");
    CHECK(mesh_refused(b) == 0 && mesh_admit(b, "Pixel", 0x0200a8c0u) == 1);
}

/* With approval on, a new device asks; a device already let in (or the console page) answers. */
static void test_asks(void) {
    static const char B[] = "02000000000000000000000000000000000000000000000000000000000000ab";
    static const char C[] = "03000000000000000000000000000000000000000000000000000000000000ab";
    char body[160];
    const char *r;
    uint8_t b[32], c[32], d[32], e[32];
    key_of(2, b);
    key_of(3, c);
    key_of(4, d);
    key_of(5, e);
    start();
    /* Approval off: let in, and how (Tailscale's 100.64.0.0/10, or the home network). */
    CHECK(mesh_admit(d, "Laptop away", 0x02014064u) == 1); /* 100.64.1.2 */
    CHECK(mesh_admit(e, "Desktop", 0x0500a8c0u) == 1);
    r = call(MESH_STATE, 1, "{}");
    CHECK(has(r, "\"asks\":[]"));
    CHECK(has(r, "\"name\":\"Laptop away\",\"nick\":\"\",\"kind\":\"\",\"os\":\"\",\"app\":\"\",\"via\":\"tailscale\""));
    CHECK(has(r, "\"name\":\"Desktop\",\"nick\":\"\",\"kind\":\"\",\"os\":\"\",\"app\":\"\",\"via\":\"network\""));
    call(MESH_SETTINGS, 1, "{\"require_approval\":true}");
    /* It asks once, however often it tries. */
    CHECK(mesh_admit(b, "Pixel", 0x0200a8c0u) == 0);
    CHECK(mesh_admit(b, "Pixel", 0x0200a8c0u) == 0);
    r = call(MESH_STATE, 1, "{}");
    CHECK(has(r, "\"asks\":[{\"id\":\"0200") && has(r, "\"name\":\"Pixel\",\"ip\":\"192.168.0.2\""));
    CHECK(has(r, "Pixel asks to connect"));
    /* Not listed among the devices until it is let in. */
    CHECK(!has(strstr(r, "\"devices\":[") ? strstr(r, "\"devices\":[") : "", "\"name\":\"Pixel\""));
    /* Allowed: it gets in on its next try, listed as approved. */
    snprintf(body, sizeof body, "{\"op\":\"allow\",\"id\":\"%s\"}", B);
    CHECK(has(call(MESH_DEVICE, 1, body), "\"ok\":true"));
    r = call(MESH_STATE, 1, "{}");
    CHECK(has(r, "\"asks\":[]") && has(r, "let in Pixel"));
    CHECK(mesh_admit(b, "Pixel", 0x0200a8c0u) == 1);
    CHECK(has(call(MESH_STATE, 1, "{}"), "\"name\":\"Pixel\",\"nick\":\"\",\"kind\":\"\",\"os\":\"\",\"app\":\"\",\"via\":\"approved\""));
    /* An answer for no ask (twice, or a made-up id) is refused. */
    CHECK(has(call(MESH_DEVICE, 1, body), "not_found"));
    /* Declined: still out, and not asked about again. */
    CHECK(mesh_admit(c, "Stranger", 0x0300a8c0u) == 0);
    snprintf(body, sizeof body, "{\"op\":\"decline\",\"id\":\"%s\"}", C);
    CHECK(has(call(MESH_DEVICE, 1, body), "\"ok\":true"));
    CHECK(mesh_admit(c, "Stranger", 0x0300a8c0u) == 0);
    r = call(MESH_STATE, 1, "{}");
    CHECK(has(r, "\"asks\":[]") && has(r, "turned away Stranger"));
    /* An ask lapses after ten minutes. */
    key_of(6, c);
    CHECK(mesh_admit(c, "Tablet", 0x0600a8c0u) == 0);
    CHECK(has(call(MESH_STATE, 1, "{}"), "\"name\":\"Tablet\",\"ip\""));
    g_mono += 11u * 60u * 1000u;
    CHECK(has(call(MESH_STATE, 1, "{}"), "\"asks\":[]"));
    /* Approval off again: nothing is left asking. */
    CHECK(mesh_admit(c, "Tablet", 0x0600a8c0u) == 0);
    call(MESH_SETTINGS, 1, "{\"require_approval\":false}");
    CHECK(has(call(MESH_STATE, 1, "{}"), "\"asks\":[]"));
    /* How each got in survives a restart. */
    reload();
    r = call(MESH_STATE, 1, "{}");
    CHECK(has(r, "\"via\":\"approved\"") && has(r, "\"via\":\"tailscale\"") && has(r, "\"via\":\"network\""));
}

/* The console page never lets a device allow itself; one address cannot fill the list of asks; an
 * Allow is not pushed out by newer asks. */
static void test_ask_limits(void) {
    static const char B[] = "02000000000000000000000000000000000000000000000000000000000000ab";
    char body[200];
    const char *r;
    uint8_t b[32], k[32];
    int i;
    start();
    call(MESH_SETTINGS, 1, "{\"require_approval\":true}");
    key_of(2, b);
    CHECK(mesh_admit(b, "Pixel", 0x0200a8c0u) == 0); /* 192.168.0.2 */
    /* Allowed from the page opened on the asking device itself: refused, still asking. */
    snprintf(body, sizeof body, "{\"op\":\"allow\",\"id\":\"%s\",\"from\":\"192.168.0.2\"}", B);
    CHECK(has(call(MESH_DEVICE, 1, body), "self_allow"));
    CHECK(has(call(MESH_STATE, 1, "{}"), "\"name\":\"Pixel\",\"ip\":\"192.168.0.2\""));
    /* From another address (or the PS5's own browser, which names none): allowed. */
    snprintf(body, sizeof body, "{\"op\":\"allow\",\"id\":\"%s\",\"from\":\"192.168.0.9\"}", B);
    CHECK(has(call(MESH_DEVICE, 1, body), "\"ok\":true"));
    /* Made-up keys from one address: one ask at a time, told once. */
    for (i = 0; i < 20; i++) {
        key_of(40 + i, k);
        CHECK(mesh_admit(k, "Spam", 0x0700a8c0u) == 0);
    }
    r = call(MESH_STATE, 1, "{}");
    CHECK(strstr(r, "\"name\":\"Spam\"") && !strstr(strstr(r, "\"name\":\"Spam\"") + 1, "\"name\":\"Spam\""));
    /* Many addresses: the list stays capped, and the Allow given above still holds. */
    for (i = 0; i < 20; i++) {
        key_of(80 + i, k);
        CHECK(mesh_admit(k, "Crowd", (uint32_t)(0x0000a8c0u | ((uint32_t)(10 + i) << 24))) == 0);
    }
    CHECK(mesh_admit(b, "Pixel", 0x0200a8c0u) == 1);
}

static void test_older_app_is_listed(void) {
    const char *r;
    start();
    online(7, 0x0700a8c0u);
    snprintf(g_on[0].name, sizeof g_on[0].name, "%s", "ps5upload");
    r = call(MESH_STATE, 1, "{}");
    CHECK(has(r, "\"id\":\"0700") && has(r, "\"name\":\"ps5upload\"") && has(r, "\"app\":\"\""));
    CHECK(!has(r, "\"kind\":\"joined\""));
    /* It updates and says hello: now it joins, under its real name. */
    r = call(MESH_HELLO, 7, "{\"name\":\"Gaming PC\",\"kind\":\"desktop\",\"app\":\"8.0.0\"}");
    CHECK(has(r, "\"new\":true"));
    CHECK(has(call(MESH_STATE, 1, "{}"), "Gaming PC connected for the first time"));
}

static void test_hello_and_state(void) {
    const char *r;
    char since[64];
    start();
    online(1, 0x0100a8c0u);
    r = call(MESH_HELLO, 1, "{\"name\":\"MacBook\",\"kind\":\"desktop\",\"os\":\"macOS 27\",\"app\":\"8.0.0\"}");
    CHECK(has(r, "\"helper\":\"8.0.0\"") && has(r, "\"new\":true") && has(r, "\"require_approval\":false"));
    r = call(MESH_STATE, 1, "{}");
    CHECK(has(r, "\"name\":\"MacBook\"") && has(r, "\"kind\":\"desktop\"") && has(r, "\"online\":true"));
    CHECK(has(r, "\"ip\":\"192.168.0.1\"") && has(r, "\"kind\":\"joined\""));
    {
        const char *p = strstr(r, "\"rev\":");
        snprintf(since, sizeof since, "{\"since\":%llu}", p ? strtoull(p + 6, NULL, 10) : 0ull);
    }
    CHECK(has(call(MESH_STATE, 1, since), "\"same\":true"));
    /* Another device coming online changes the picture. */
    online(2, 0x0200a8c0u);
    call(MESH_HELLO, 2, "{\"name\":\"Pixel 9\",\"kind\":\"android\"}");
    r = call(MESH_STATE, 1, since);
    CHECK(!has(r, "\"same\":true") && has(r, "Pixel 9"));
    /* Presence shows only while online, and is validated JSON. */
    CHECK(has(call(MESH_ACTIVITY, 2, "{\"items\":[{\"kind\":\"upload\",\"title\":\"Astro Bot\",\"pct\":42}]}"),
              "\"ok\":true"));
    CHECK(has(call(MESH_STATE, 1, "{}"), "\"title\":\"Astro Bot\""));
    CHECK(has(call(MESH_ACTIVITY, 2, "{\"items\":{\"x\":1}}"), "bad_request"));
    CHECK(has(call(MESH_ACTIVITY, 2, "{\"items\":[1,2"), "bad_request"));
    g_non = 1; /* the phone went to sleep */
    r = call(MESH_STATE, 1, "{}");
    CHECK(!has(r, "Astro Bot") && has(r, "\"online\":false"));
    /* Two phones with one name on a reused address are two devices. */
    online(3, 0x0200a8c0u);
    g_nforgot = 0;
    call(MESH_HELLO, 3, "{\"name\":\"Pixel 9\",\"kind\":\"android\"}");
    r = call(MESH_STATE, 1, "{}");
    CHECK(g_nforgot == 0 && has(r, "\"id\":\"0200") && has(r, "\"id\":\"0300"));
    /* A Docker engine back under a new key replaces its old entry, and its old key stays trusted. */
    g_non = 1;
    online(4, 0x0400a8c0u);
    call(MESH_HELLO, 4, "{\"name\":\"Docker\",\"kind\":\"docker\"}");
    g_non = 1;
    online(5, 0x0400a8c0u);
    call(MESH_HELLO, 5, "{\"name\":\"Docker\",\"kind\":\"docker\"}");
    r = call(MESH_STATE, 1, "{}");
    CHECK(g_nforgot == 0 && !has(r, "\"id\":\"0400") && has(r, "\"id\":\"0500"));
    /* Rename sets the nickname shown to everyone. */
    call(MESH_DEVICE, 1, "{\"op\":\"rename\",\"id\":\"03000000000000000000000000000000000000000000000000000000000000ab\",\"name\":\"My phone\"}");
    CHECK(has(call(MESH_STATE, 1, "{}"), "\"nick\":\"My phone\""));
}

static void test_locks(void) {
    const char *r;
    start();
    online(1, 1);
    online(2, 2);
    call(MESH_HELLO, 1, "{\"name\":\"Mac\"}");
    call(MESH_HELLO, 2, "{\"name\":\"Phone\"}");
    r = call(MESH_LOCK, 1, "{\"op\":\"take\",\"name\":\"install:PPSA01234\",\"label\":\"Installing Astro Bot\"}");
    CHECK(has(r, "\"got\":true"));
    r = call(MESH_LOCK, 2, "{\"op\":\"take\",\"name\":\"install:PPSA01234\"}");
    CHECK(has(r, "\"got\":false") && has(r, "\"by_name\":\"Mac\"") && has(r, "Installing Astro Bot"));
    CHECK(has(call(MESH_LOCK, 1, "{\"op\":\"take\",\"name\":\"install:PPSA01234\"}"), "\"got\":true")); /* renew */
    r = call(MESH_LOCK, 2, "{\"op\":\"take\",\"name\":\"install:PPSA01234\",\"force\":true}");
    CHECK(has(r, "\"got\":true"));
    CHECK(has(call(MESH_STATE, 1, "{}"), "\"kind\":\"takeover\""));
    /* The holder leaves: the lock lapses. */
    g_non = 1; /* only device 1 online */
    CHECK(has(call(MESH_LOCK, 1, "{\"op\":\"take\",\"name\":\"install:PPSA01234\"}"), "\"got\":true"));
    /* The console's date jumps a day ahead (set by hand, NTP): the lock does not lapse. */
    g_now += 86400000ull;
    g_non = 2;
    CHECK(has(call(MESH_LOCK, 2, "{\"op\":\"take\",\"name\":\"install:PPSA01234\"}"), "\"got\":false"));
    g_non = 1;
    /* Time runs out. */
    g_now += 61000;
    g_mono += 61000;
    online(2, 2);
    CHECK(has(call(MESH_LOCK, 2, "{\"op\":\"take\",\"name\":\"install:PPSA01234\"}"), "\"got\":true"));
    /* Release only by the holder (or forced). */
    call(MESH_LOCK, 1, "{\"op\":\"release\",\"name\":\"install:PPSA01234\"}");
    CHECK(has(call(MESH_LOCK, 1, "{\"op\":\"take\",\"name\":\"install:PPSA01234\"}"), "\"got\":false"));
    call(MESH_LOCK, 2, "{\"op\":\"release\",\"name\":\"install:PPSA01234\"}");
    CHECK(has(call(MESH_LOCK, 1, "{\"op\":\"take\",\"name\":\"install:PPSA01234\"}"), "\"got\":true"));
    /* Two windows of one device (browsers of a Docker engine) are two holders. */
    CHECK(has(call(MESH_LOCK, 1, "{\"op\":\"take\",\"name\":\"w\",\"owner\":\"tab1\"}"), "\"got\":true"));
    r = call(MESH_LOCK, 1, "{\"op\":\"take\",\"name\":\"w\",\"owner\":\"tab2\"}");
    CHECK(has(r, "\"got\":false") && has(r, "\"same_device\":true"));
    call(MESH_LOCK, 1, "{\"op\":\"release\",\"name\":\"w\",\"owner\":\"tab2\"}"); /* not its lock */
    CHECK(has(call(MESH_LOCK, 1, "{\"op\":\"take\",\"name\":\"w\",\"owner\":\"tab2\"}"), "\"got\":false"));
    call(MESH_LOCK, 1, "{\"op\":\"release\",\"name\":\"w\",\"owner\":\"tab1\"}");
    CHECK(has(call(MESH_LOCK, 1, "{\"op\":\"take\",\"name\":\"w\",\"owner\":\"tab2\"}"), "\"got\":true"));
    CHECK(has(call(MESH_LOCK, 1, "{\"op\":\"steal\",\"name\":\"x\"}"), "bad_op"));
    CHECK(has(call(MESH_LOCK, 1, "{\"op\":\"take\"}"), "bad_request"));
}

static void test_docs(void) {
    const char *r;
    start();
    r = call(MESH_DOC_GET, 1, "{\"name\":\"user\"}");
    CHECK(has(r, "\"rev\":0") && has(r, "\"doc\":null"));
    r = call(MESH_DOC_PUT, 1, "{\"name\":\"user\",\"base\":0,\"doc\":{\"keys\":{\"a\":{\"v\":\"1\",\"at\":5}}}}");
    CHECK(has(r, "\"saved\":true") && has(r, "\"rev\":1"));
    r = call(MESH_DOC_GET, 2, "{\"name\":\"user\"}");
    CHECK(has(r, "\"rev\":1") && has(r, "{\"keys\":{\"a\":{\"v\":\"1\",\"at\":5}}}"));
    /* A write against an old revision is answered with what is there. */
    r = call(MESH_DOC_PUT, 2, "{\"name\":\"user\",\"base\":0,\"doc\":{\"keys\":{}}}");
    CHECK(has(r, "\"saved\":false") && has(r, "\"rev\":1") && has(r, "\"a\":{\"v\":\"1\""));
    CHECK(has(call(MESH_DOC_PUT, 2, "{\"name\":\"../x\",\"base\":0,\"doc\":{}}"), "bad_request"));
    CHECK(has(call(MESH_DOC_PUT, 2, "{\"name\":\"user\",\"base\":1,\"doc\":[1]}"), "bad_request"));
    CHECK(has(call(MESH_DOC_PUT, 2, "{\"name\":\"user\",\"base\":1,\"doc\":{\"a\":}}"), "bad_request"));
    /* Survives a restart. */
    reload();
    CHECK(has(call(MESH_DOC_GET, 1, "{\"name\":\"user\"}"), "\"rev\":1"));
}

static void test_persist_and_prune(void) {
    const char *r;
    start();
    online(5, 5);
    call(MESH_HELLO, 5, "{\"name\":\"Old laptop\",\"kind\":\"desktop\"}");
    call(MESH_SETTINGS, 5, "{\"prune_days\":30}");
    reload(); /* from disk */
    r = call(MESH_STATE, 1, "{}");
    CHECK(has(r, "Old laptop") && has(r, "\"prune_days\":30"));
    g_now += 31ull * 86400000ull; /* a month later, nobody online */
    g_nforgot = 0;
    r = call(MESH_STATE, 1, "{}");
    CHECK(!has(r, "Old laptop"));
    CHECK(g_nforgot >= 1);
    call(MESH_SETTINGS, 1, "{\"prune_days\":0}");
    CHECK(has(call(MESH_STATE, 1, "{}"), "\"prune_days\":0"));
}

static void test_events(void) {
    const char *r;
    start();
    online(1, 1);
    call(MESH_HELLO, 1, "{\"name\":\"Mac\"}");
    r = call(MESH_ANNOUNCE, 1, "{\"kind\":\"restart\",\"text\":\"sending helper 8.0.0\"}");
    CHECK(has(r, "\"ok\":true"));
    r = call(MESH_STATE, 1, "{}");
    CHECK(has(r, "\"kind\":\"restart\"") && has(r, "Mac: sending helper 8.0.0"));
    {
        const char *p = strstr(r, "\"ev\":");
        char q[64];
        snprintf(q, sizeof q, "{\"ev\":%llu}", p ? strtoull(p + 5, NULL, 10) : 0ull);
        r = call(MESH_STATE, 1, q);
        CHECK(!has(r, "sending helper")); /* already seen */
    }
    CHECK(has(call(99, 1, "{}"), "bad_method"));
}

int main(void) {
    snprintf(g_dir, sizeof g_dir, "/tmp/mesh-selftest-XXXXXX");
    if (!mkdtemp(g_dir)) {
        perror("mkdtemp");
        return 2;
    }
    test_admission();
    test_asks();
    test_ask_limits();
    test_older_app_is_listed();
    test_hello_and_state();
    test_locks();
    test_docs();
    test_persist_and_prune();
    test_events();
    {
        char cmd[300];
        snprintf(cmd, sizeof cmd, "rm -rf '%s'", g_dir);
        if (system(cmd) != 0) fprintf(stderr, "could not remove %s\n", g_dir);
    }
    if (failures) {
        fprintf(stderr, "%d mesh check(s) failed\n", failures);
        return 1;
    }
    printf("mesh selftest ok\n");
    return 0;
}
