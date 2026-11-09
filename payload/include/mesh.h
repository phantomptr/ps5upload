/* The devices that use this console, and what they share through it (8.0.0, the multi-device mesh).
 *
 * Every phone, computer and Docker engine that connects is a device. The console keeps:
 *   - the device list (MESH_DIR/v1/devices): name, kind, OS, app version, first/last seen, address,
 *     blocked; the user can rename, remove or block one;
 *   - settings (MESH_DIR/v1/settings): require approval for new devices, forget unseen devices after N
 *     days;
 *   - with approval on, the devices that asked to connect (memory only, 10 minutes each): any device
 *     already let in, or the console's own page, answers Allow (the device gets in on its next try)
 *     or Decline; how each listed device was let in (MESH_DIR/v1/via);
 *   - what each online device is doing (presence, memory only), locks that keep two devices from the
 *     same install / folder / power action (memory only, they lapse), and a short event list;
 *   - shared documents (MESH_DIR/v1/doc-<name>): opaque JSON objects with a revision, written only
 *     against the revision the writer read (the engine merges and retries).
 *
 * Every reply is JSON. A refusal is {"ok":false,"err":"<token>"}; a lock someone else holds or a stale
 * document write are answers, not refusals ({"ok":true,"got":false,...}, {"ok":true,"saved":false,...}).
 * The trust store itself (AVA1 peers) belongs to the AVA1 server; this module asks it to forget a key
 * through the hooks and answers its admission questions.
 *
 * Thread safety: one lock inside; hooks are always called with it released. */
#ifndef PS5UPLOAD_MESH_H
#define PS5UPLOAD_MESH_H

#include <stddef.h>
#include <stdint.h>

#define MESH_DIR "/data/ps5upload/mesh"
#define MESH_MAX_DEVICES 64
#define MESH_ACTIVITY_MAX 1536u        /* one device's presence (a JSON array) */
#define MESH_DOC_MAX (160u * 1024u)    /* one shared document */
#define MESH_DOCS_MAX 32
#define MESH_LOCKS_MAX 64
#define MESH_ASKS_MAX 8 /* devices waiting for the owner's Allow / Decline */
#define MESH_EVENTS_MAX 32
#define MESH_REQ_MAX (MESH_DOC_MAX + 1024u)
#define MESH_REPLY_MAX (200u * 1024u)
#define MESH_PRUNE_DEFAULT_DAYS 90u

typedef struct {
    uint8_t key[32];
    uint32_t ip;   /* network order */
    char name[64]; /* from the handshake: listed for a device that never says hello (an older app) */
} mesh_online_t;

typedef struct {
    /* Devices with a live session now; returns how many (<= max). */
    int (*online)(mesh_online_t *out, int max);
    /* Remove a key from the trust store and end its sessions; 0 or -1. */
    int (*forget)(const uint8_t key[32]);
    /* Wall-clock milliseconds since 1970. */
    uint64_t (*now_ms)(void);
    /* Milliseconds on a clock that never jumps (lock expiry); NULL: CLOCK_MONOTONIC. */
    uint64_t (*mono_ms)(void);
} mesh_hooks_t;

/* Wall-clock milliseconds since 1970: the clock the device list's dates use. */
uint64_t mesh_wall_ms(void);

/* Loads the device list and settings from dir (created when missing). 0, or -1 when dir cannot be
 * made (the module then keeps everything in memory). Calling it again replaces the state (tests). */
int mesh_start(const char *dir, const mesh_hooks_t *hooks, const char *helper_version);

/* Handshake admission for a key that is not trusted yet: 1 trust it (added to the list), 0 the pairing
 * rules (approval is required: the device is listed as asking until someone allows or declines it;
 * once allowed, this answers 1), -1 refuse (blocked). */
int mesh_admit(const uint8_t key[32], const char *name, uint32_t ip);
/* Nonzero when key is blocked. */
int mesh_refused(const uint8_t key[32]);

enum {
    MESH_HELLO = 1,    /* {name,kind,os,app,proto} -> {you,helper,rev,settings,new} */
    MESH_STATE = 2,    /* {since,ev} -> {rev,same} | the whole picture (devices, asks, locks, events) */
    MESH_ACTIVITY = 3, /* {items:[...]} -> {ok,rev} */
    MESH_LOCK = 4,     /* {op:take|release,name,ttl,force,label} -> {ok,got,...} */
    MESH_DEVICE = 5,   /* {op:rename|remove|block|unblock|allow|decline,id,name} -> {ok} */
    MESH_SETTINGS = 6, /* {require_approval?,prune_days?} -> settings */
    MESH_DOC_GET = 7,  /* {name} -> {rev,doc} */
    MESH_DOC_PUT = 8,  /* {name,base,doc} -> {ok,saved,rev[,doc]} */
    MESH_ANNOUNCE = 9, /* {kind,text} -> {ok,rev} */
};

/* Runs one call as `caller`. Writes the JSON reply into out (NUL-terminated) and returns its length,
 * or -1 when even a refusal does not fit. */
int mesh_call(int which, const uint8_t caller[32], const char *body, size_t n, char *out, size_t cap);

#endif
