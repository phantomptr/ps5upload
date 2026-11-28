/* Play Rooms' hosted-by marker and PS5Upload Play's one-time pairing grants: see
 * include/play_host.h. */
#include "play_host.h"

#include <pthread.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include "webui_codec.h"

typedef struct {
    char name[PLAY_ROOM_PLAYER_NAME_MAX + 1];
    int ping; /* -1: not known */
    int seat; /* 0: not said */
    int bars; /* 0..4 (the rooms design's tier: 4 Excellent .. 1 Poor, 0 no reading); -1 not said */
    char via[16];
} player_t;

typedef struct {
    int used;
    uint8_t key[32];
    uint64_t at_ms, until_ms;
    char name[PLAY_ROOM_NAME_MAX + 1];
    char visibility[8];
    int locked, chat, voice;
    int has_game;
    char game[PLAY_ROOM_GAME_MAX + 1];
    char title_id[16];
    int size, playing, asks;
    char from[PLAY_ROOM_FROM_MAX + 1];
    int np;
    player_t p[PLAY_ROOM_PLAYERS];
} room_t;

typedef struct {
    int used;
    uint8_t key[32];
    char grant[65];
    uint64_t until_ms;
} grant_t;

static pthread_mutex_t g_mu = PTHREAD_MUTEX_INITIALIZER;
static room_t g_rooms[PLAY_HOST_ENGINES];
static grant_t g_grants[PLAY_HOST_ENGINES * 2];
static play_host_lookup_fn g_lookup;
static char g_intent[8];
static uint64_t g_intent_until;

void play_host_set_lookup(play_host_lookup_fn fn) {
    pthread_mutex_lock(&g_mu);
    g_lookup = fn;
    pthread_mutex_unlock(&g_mu);
}

void play_host_reset(void) {
    pthread_mutex_lock(&g_mu);
    memset(g_rooms, 0, sizeof g_rooms);
    memset(g_grants, 0, sizeof g_grants);
    g_intent[0] = 0;
    pthread_mutex_unlock(&g_mu);
}

int play_intent_set(const char *intent, uint64_t now_ms) {
    if (!intent || (strcmp(intent, "join") != 0 && strcmp(intent, "host") != 0 && strcmp(intent, "rooms") != 0)) return -1;
    pthread_mutex_lock(&g_mu);
    snprintf(g_intent, sizeof g_intent, "%s", intent);
    g_intent_until = now_ms + PLAY_INTENT_TTL_S * 1000u;
    pthread_mutex_unlock(&g_mu);
    return 0;
}

int play_intent_take_json(uint64_t now_ms, char *out, size_t cap) {
    char got[8] = "";
    int k;
    pthread_mutex_lock(&g_mu);
    if (g_intent[0] && now_ms < g_intent_until) snprintf(got, sizeof got, "%s", g_intent);
    g_intent[0] = 0;
    pthread_mutex_unlock(&g_mu);
    k = got[0] ? snprintf(out, cap, "{\"intent\":\"%s\"}", got) : snprintf(out, cap, "{\"intent\":null}");
    return k < 0 || (size_t)k >= cap ? -1 : k;
}

/* ---- a little JSON: one key's value span, at the top level of an object ---- */

static int ws(char c) { return c == ' ' || c == '\t' || c == '\n' || c == '\r'; }

static const char *skip_value(const char *p, const char *e) {
    if (p >= e) return NULL;
    if (*p == '"') {
        for (p++; p < e; p++) {
            if (*p == '\\') {
                if (++p >= e) return NULL;
            } else if (*p == '"') {
                return p + 1;
            }
        }
        return NULL;
    }
    if (*p == '{' || *p == '[') {
        int depth = 0;
        while (p < e) {
            if (*p == '"') {
                p = skip_value(p, e);
                if (!p) return NULL;
                continue;
            }
            if (*p == '{' || *p == '[') depth++;
            else if ((*p == '}' || *p == ']') && --depth == 0) return p + 1;
            p++;
        }
        return NULL;
    }
    while (p < e && *p != ',' && *p != ']' && *p != '}' && !ws(*p)) p++;
    return p;
}

/* The value of top-level `key` in object json: 1 (span in *v, *vl), 0 absent, -1 not an object. */
static int top_value(const char *json, size_t n, const char *key, const char **v, size_t *vl) {
    const char *p = json, *e = json + n;
    size_t kl = strlen(key);
    while (p < e && ws(*p)) p++;
    if (p >= e || *p != '{') return -1;
    p++;
    for (;;) {
        const char *ks, *ke, *vs, *ve;
        while (p < e && ws(*p)) p++;
        if (p < e && *p == '}') return 0;
        if (p >= e || *p != '"') return -1;
        ks = p + 1;
        ke = skip_value(p, e);
        if (!ke) return -1;
        p = ke;
        while (p < e && ws(*p)) p++;
        if (p >= e || *p != ':') return -1;
        p++;
        while (p < e && ws(*p)) p++;
        vs = p;
        ve = skip_value(p, e);
        if (!ve || ve == vs) return -1;
        if ((size_t)(ke - 1 - ks) == kl && memcmp(ks, key, kl) == 0) {
            *v = vs;
            *vl = (size_t)(ve - vs);
            return 1;
        }
        p = ve;
        while (p < e && ws(*p)) p++;
        if (p < e && *p == ',') {
            p++;
            continue;
        }
        if (p < e && *p == '}') return 0;
        return -1;
    }
}

/* The next element of the array span arr: 1 (*el, *el_len), 0 at its end, -1 malformed. */
static int array_next(const char *arr, size_t len, size_t *pos, const char **el, size_t *el_len) {
    const char *e = arr + len, *p, *q;
    if (*pos >= len) return 0;
    p = arr + (*pos ? *pos : 0);
    if (*pos == 0) {
        while (p < e && ws(*p)) p++;
        if (p >= e || *p != '[') return -1;
        p++;
    } else {
        while (p < e && ws(*p)) p++;
        if (p < e && *p == ']') {
            *pos = len;
            return 0;
        }
        if (p >= e || *p != ',') return -1;
        p++;
    }
    while (p < e && ws(*p)) p++;
    if (p < e && *p == ']') {
        *pos = len;
        return 0;
    }
    q = skip_value(p, e);
    if (!q || q == p) return -1;
    *el = p;
    *el_len = (size_t)(q - p);
    *pos = (size_t)(q - arr);
    return 1;
}

/* A string field cut to cap - 1 bytes at a character boundary, control characters dropped. 1 a
 * non-empty string, 0 absent/empty/not a string. */
static int text_field(const char *json, size_t n, const char *key, char *out, size_t cap) {
    char *tmp;
    size_t i, k = 0, len;
    int r;
    out[0] = 0;
    if (n > PLAY_HOST_REQ_MAX) return 0;
    tmp = malloc(n + 1);
    if (!tmp) return 0;
    r = webui_json_str(json, n, key, tmp, n + 1);
    if (r != 1) {
        free(tmp);
        return 0;
    }
    len = strlen(tmp);
    i = 0;
    while (i < len && tmp[i] == ' ') i++; /* leading spaces never count toward the bound */
    for (; i < len && k + 1 < cap; i++) {
        unsigned char c = (unsigned char)tmp[i];
        if (c < 0x20 || c == 0x7f) continue;
        out[k++] = (char)c;
    }
    /* never half a character: drop a cut sequence's lead and continuation bytes */
    if (i < len) {
        while (k > 0 && ((unsigned char)out[k - 1] & 0xC0u) == 0x80u) k--;
        if (k > 0 && (unsigned char)out[k - 1] >= 0xC0u) k--;
    }
    out[k] = 0;
    while (k > 0 && out[k - 1] == ' ') out[--k] = 0;
    free(tmp);
    return out[0] != 0;
}

static int int_field(const char *json, size_t n, const char *key, long long lo, long long hi, long long dflt) {
    long long v;
    if (webui_json_i64(json, n, key, &v) != 1 || v < lo || v > hi) return (int)dflt;
    return (int)v;
}

static int bool_field(const char *json, size_t n, const char *key) {
    int v = 0;
    return webui_json_bool(json, n, key, &v) == 1 && v;
}

/* ---- play.room ---- */

static int answer(size_t cap, int n) { return n < 0 || (size_t)n >= cap ? -1 : n; }

static int refuse(char *out, size_t cap, const char *err) {
    return answer(cap, snprintf(out, cap, "{\"ok\":false,\"err\":\"%s\"}", err));
}

static room_t *find_room(const uint8_t key[32]) {
    for (int i = 0; i < PLAY_HOST_ENGINES; i++)
        if (g_rooms[i].used && memcmp(g_rooms[i].key, key, 32) == 0) return &g_rooms[i];
    return NULL;
}

static void expire_locked(uint64_t now_ms) {
    for (int i = 0; i < PLAY_HOST_ENGINES; i++)
        if (g_rooms[i].used && now_ms >= g_rooms[i].until_ms) memset(&g_rooms[i], 0, sizeof g_rooms[i]);
    for (size_t i = 0; i < sizeof g_grants / sizeof g_grants[0]; i++)
        if (g_grants[i].used && now_ms >= g_grants[i].until_ms) memset(&g_grants[i], 0, sizeof g_grants[i]);
}

static int count_rooms_locked(void) {
    int n = 0;
    for (int i = 0; i < PLAY_HOST_ENGINES; i++) n += g_rooms[i].used;
    return n;
}

static int parse_room(const char *r, size_t rl, room_t *out) {
    const char *gv, *pv;
    size_t gl, pl;
    char vis[16];
    memset(out, 0, sizeof *out);
    if (!text_field(r, rl, "name", out->name, sizeof out->name)) return -1;
    text_field(r, rl, "visibility", vis, sizeof vis);
    snprintf(out->visibility, sizeof out->visibility, "%s",
             !strcmp(vis, "public") ? "public" : !strcmp(vis, "friends") ? "friends" : "private");
    out->locked = bool_field(r, rl, "locked");
    out->chat = bool_field(r, rl, "chat");
    out->voice = bool_field(r, rl, "voice");
    if (top_value(r, rl, "game", &gv, &gl) == 1 && gv[0] == '{') {
        char tid[24];
        out->has_game = text_field(gv, gl, "name", out->game, sizeof out->game);
        if (text_field(gv, gl, "title_id", tid, sizeof tid) && webui_is_game_title_id(tid)) {
            snprintf(out->title_id, sizeof out->title_id, "%s", tid);
            out->has_game = 1;
        }
    }
    out->size = int_field(r, rl, "size", 1, 8, 0);
    out->asks = int_field(r, rl, "asks", 0, 999, 0);
    text_field(r, rl, "hosted_from", out->from, sizeof out->from);
    if (top_value(r, rl, "players", &pv, &pl) == 1 && pv[0] == '[') {
        size_t pos = 0, el_len;
        const char *el;
        int k, total = 0;
        while ((k = array_next(pv, pl, &pos, &el, &el_len)) == 1) {
            player_t *p;
            if (el[0] != '{') continue;
            total++;
            if (out->np >= PLAY_ROOM_PLAYERS) continue;
            p = &out->p[out->np];
            if (!text_field(el, el_len, "name", p->name, sizeof p->name)) snprintf(p->name, sizeof p->name, "?");
            p->ping = int_field(el, el_len, "ping_ms", 0, 60000, -1);
            p->seat = int_field(el, el_len, "seat", 1, 8, 0);
            p->bars = int_field(el, el_len, "bars", 0, 4, -1);
            text_field(el, el_len, "via", p->via, sizeof p->via);
            for (char *c = p->via; *c; c++)
                if (!((*c >= 'a' && *c <= 'z') || (*c >= '0' && *c <= '9') || *c == '_')) {
                    p->via[0] = 0;
                    break;
                }
            out->np++;
        }
        if (k < 0) return -1;
        out->playing = total;
    }
    out->playing = int_field(r, rl, "playing", 0, 99, out->playing);
    if (out->playing < out->np) out->playing = out->np;
    return 0;
}

int play_room_set_json(const uint8_t caller[32], const char *body, size_t n, uint64_t now_ms, char *out, size_t cap) {
    const char *rv;
    size_t rl;
    uint64_t ttl = PLAY_ROOM_TTL_DEFAULT_S;
    room_t parsed, *slot;
    int r, held;
    if (!caller || !body || n > PLAY_HOST_REQ_MAX) return refuse(out, cap, "bad_request");
    r = top_value(body, n, "room", &rv, &rl);
    if (r < 0) return refuse(out, cap, "bad_request");
    if (webui_json_u64(body, n, "ttl_s", &ttl) == 1) {
        if (ttl < PLAY_ROOM_TTL_MIN_S) ttl = PLAY_ROOM_TTL_MIN_S;
        if (ttl > PLAY_ROOM_TTL_MAX_S) ttl = PLAY_ROOM_TTL_MAX_S;
    } else {
        ttl = PLAY_ROOM_TTL_DEFAULT_S;
    }
    if (r == 0 || (rl == 4 && memcmp(rv, "null", 4) == 0)) {
        /* cleared (or never sent: the same) */
        pthread_mutex_lock(&g_mu);
        slot = find_room(caller);
        if (slot) memset(slot, 0, sizeof *slot);
        expire_locked(now_ms);
        held = count_rooms_locked();
        pthread_mutex_unlock(&g_mu);
        return answer(cap, snprintf(out, cap, "{\"ok\":true,\"rooms\":%d}", held));
    }
    if (rv[0] != '{' || parse_room(rv, rl, &parsed) != 0) return refuse(out, cap, "bad_request");
    memcpy(parsed.key, caller, 32);
    parsed.used = 1;
    parsed.at_ms = now_ms;
    parsed.until_ms = now_ms + ttl * 1000u;
    pthread_mutex_lock(&g_mu);
    expire_locked(now_ms);
    slot = find_room(caller);
    for (int i = 0; !slot && i < PLAY_HOST_ENGINES; i++)
        if (!g_rooms[i].used) slot = &g_rooms[i];
    if (!slot) {
        pthread_mutex_unlock(&g_mu);
        return refuse(out, cap, "rooms_full");
    }
    *slot = parsed;
    held = count_rooms_locked();
    pthread_mutex_unlock(&g_mu);
    return answer(cap, snprintf(out, cap, "{\"ok\":true,\"rooms\":%d}", held));
}

/* ---- writing ---- */

typedef struct {
    char *out;
    size_t len, cap;
    int err;
} buf_t;

static void put(buf_t *b, const char *s) {
    size_t l = strlen(s);
    if (b->err || b->len + l >= b->cap) {
        b->err = 1;
        return;
    }
    memcpy(b->out + b->len, s, l + 1);
    b->len += l;
}

static void putf(buf_t *b, const char *fmt, long long v) {
    char t[32];
    snprintf(t, sizeof t, fmt, v);
    put(b, t);
}

static void putstr(buf_t *b, const char *s) {
    char e[PLAY_ROOM_GAME_MAX * 6 + 8];
    webui_json_escape(e, sizeof e, s, strlen(s));
    put(b, "\"");
    put(b, e);
    put(b, "\"");
}

static void to_hex(const uint8_t *k, char out[65]) {
    static const char d[] = "0123456789abcdef";
    for (int i = 0; i < 32; i++) {
        out[2 * i] = d[k[i] >> 4];
        out[2 * i + 1] = d[k[i] & 15];
    }
    out[64] = 0;
}

static void put_room(buf_t *b, const room_t *r, uint64_t now_ms, const mesh_engine_t *who, int known) {
    char hex[65];
    to_hex(r->key, hex);
    put(b, "{\"engine_id\":");
    putstr(b, hex);
    put(b, ",\"hosted_from\":");
    putstr(b, r->from[0] ? r->from : known && who->name[0] ? who->name : "");
    put(b, ",\"kind\":");
    putstr(b, known ? who->kind : "");
    put(b, known && who->online ? ",\"online\":true" : ",\"online\":false");
    put(b, ",\"name\":");
    putstr(b, r->name);
    put(b, ",\"visibility\":");
    putstr(b, r->visibility);
    put(b, r->locked ? ",\"locked\":true" : ",\"locked\":false");
    if (r->has_game) {
        put(b, ",\"game\":{\"name\":");
        if (r->game[0]) putstr(b, r->game);
        else put(b, "null");
        put(b, ",\"title_id\":");
        if (r->title_id[0]) putstr(b, r->title_id);
        else put(b, "null");
        put(b, "}");
    } else {
        put(b, ",\"game\":null");
    }
    if (r->size) putf(b, ",\"size\":%lld", r->size);
    else put(b, ",\"size\":null");
    putf(b, ",\"playing\":%lld", r->playing);
    putf(b, ",\"asks\":%lld", r->asks);
    put(b, r->chat ? ",\"chat\":true" : ",\"chat\":false");
    put(b, r->voice ? ",\"voice\":true" : ",\"voice\":false");
    put(b, ",\"players\":[");
    for (int i = 0; i < r->np; i++) {
        const player_t *p = &r->p[i];
        if (i) put(b, ",");
        put(b, "{\"name\":");
        putstr(b, p->name);
        if (p->ping >= 0) putf(b, ",\"ping_ms\":%lld", p->ping);
        else put(b, ",\"ping_ms\":null");
        if (p->seat) putf(b, ",\"seat\":%lld", p->seat);
        else put(b, ",\"seat\":null");
        if (p->bars >= 0) putf(b, ",\"bars\":%lld", p->bars);
        else put(b, ",\"bars\":null");
        put(b, ",\"via\":");
        putstr(b, p->via);
        put(b, "}");
    }
    putf(b, "],\"age_s\":%lld}", (long long)((now_ms - r->at_ms) / 1000u));
}

int play_room_json(uint64_t now_ms, char *out, size_t cap) {
    buf_t b = {out, 0, cap, 0};
    room_t rooms[PLAY_HOST_ENGINES];
    mesh_engine_t who[PLAY_HOST_ENGINES];
    int known[PLAY_HOST_ENGINES], n = 0, newest = -1;
    play_host_lookup_fn lookup;
    if (!out || cap < 32) return -1;
    pthread_mutex_lock(&g_mu);
    expire_locked(now_ms);
    for (int i = 0; i < PLAY_HOST_ENGINES; i++)
        if (g_rooms[i].used) rooms[n++] = g_rooms[i];
    lookup = g_lookup;
    pthread_mutex_unlock(&g_mu);
    /* the names outside the lock (the mesh has its own) */
    for (int i = 0; i < n; i++) {
        known[i] = lookup ? lookup(rooms[i].key, &who[i]) : 0;
        if (!known[i]) memset(&who[i], 0, sizeof who[i]);
        if (newest < 0 || rooms[i].at_ms > rooms[newest].at_ms) newest = i;
    }
    put(&b, "{\"room\":");
    if (newest >= 0) put_room(&b, &rooms[newest], now_ms, &who[newest], known[newest]);
    else put(&b, "null");
    put(&b, ",\"rooms\":[");
    for (int i = 0; i < n; i++) {
        if (i) put(&b, ",");
        put_room(&b, &rooms[i], now_ms, &who[i], known[i]);
    }
    put(&b, "]}");
    return b.err ? -1 : (int)b.len;
}

int play_room_active(uint64_t now_ms, int *playing) {
    int n, newest = -1;
    pthread_mutex_lock(&g_mu);
    expire_locked(now_ms);
    n = count_rooms_locked();
    for (int i = 0; i < PLAY_HOST_ENGINES; i++)
        if (g_rooms[i].used && (newest < 0 || g_rooms[i].at_ms > g_rooms[newest].at_ms)) newest = i;
    if (playing) *playing = newest >= 0 ? g_rooms[newest].playing : 0;
    pthread_mutex_unlock(&g_mu);
    return n;
}

/* ---- play.grant ---- */

static int hex64(const char *s) {
    if (strlen(s) != 64) return 0;
    for (int i = 0; i < 64; i++)
        if (!((s[i] >= '0' && s[i] <= '9') || (s[i] >= 'a' && s[i] <= 'f'))) return 0;
    return 1;
}

int play_grant_set_json(const uint8_t caller[32], const char *body, size_t n, uint64_t now_ms, char *out, size_t cap) {
    char g[80];
    uint64_t ttl = PLAY_GRANT_TTL_DEFAULT_S;
    grant_t *slot = NULL;
    int r;
    if (!caller || !body || n > PLAY_HOST_REQ_MAX) return refuse(out, cap, "bad_request");
    if (webui_json_null(body, n, "grant")) {
        pthread_mutex_lock(&g_mu);
        for (size_t i = 0; i < sizeof g_grants / sizeof g_grants[0]; i++)
            if (g_grants[i].used && memcmp(g_grants[i].key, caller, 32) == 0) memset(&g_grants[i], 0, sizeof g_grants[i]);
        pthread_mutex_unlock(&g_mu);
        return answer(cap, snprintf(out, cap, "{\"ok\":true,\"expires_s\":0}"));
    }
    r = webui_json_str(body, n, "grant", g, sizeof g);
    for (char *c = g; r == 1 && *c; c++)
        if (*c >= 'A' && *c <= 'F') *c = (char)(*c - 'A' + 'a');
    if (r != 1 || !hex64(g)) return refuse(out, cap, "bad_request");
    if (webui_json_u64(body, n, "ttl_s", &ttl) == 1) {
        if (ttl < PLAY_GRANT_TTL_MIN_S) ttl = PLAY_GRANT_TTL_MIN_S;
        if (ttl > PLAY_GRANT_TTL_MAX_S) ttl = PLAY_GRANT_TTL_MAX_S;
    } else {
        ttl = PLAY_GRANT_TTL_DEFAULT_S;
    }
    pthread_mutex_lock(&g_mu);
    expire_locked(now_ms);
    for (size_t i = 0; i < sizeof g_grants / sizeof g_grants[0]; i++)
        if (g_grants[i].used && memcmp(g_grants[i].key, caller, 32) == 0) slot = &g_grants[i];
    for (size_t i = 0; !slot && i < sizeof g_grants / sizeof g_grants[0]; i++)
        if (!g_grants[i].used) slot = &g_grants[i];
    if (!slot) {
        /* full: the one closest to its end makes room (a grant is only an offer, renewed) */
        slot = &g_grants[0];
        for (size_t i = 1; i < sizeof g_grants / sizeof g_grants[0]; i++)
            if (g_grants[i].until_ms < slot->until_ms) slot = &g_grants[i];
    }
    memset(slot, 0, sizeof *slot);
    slot->used = 1;
    memcpy(slot->key, caller, 32);
    snprintf(slot->grant, sizeof slot->grant, "%s", g);
    slot->until_ms = now_ms + ttl * 1000u;
    pthread_mutex_unlock(&g_mu);
    return answer(cap, snprintf(out, cap, "{\"ok\":true,\"expires_s\":%llu}", (unsigned long long)ttl));
}

int play_grants_take_json(uint64_t now_ms, char *out, size_t cap) {
    grant_t got[PLAY_HOST_ENGINES * 2];
    int n = 0;
    play_host_lookup_fn lookup;
    buf_t b = {out, 0, cap, 0};
    if (!out || cap < 32) return -1;
    pthread_mutex_lock(&g_mu);
    expire_locked(now_ms);
    for (size_t i = 0; i < sizeof g_grants / sizeof g_grants[0]; i++)
        if (g_grants[i].used) {
            got[n++] = g_grants[i];
            /* one time: handed out once, then gone (the engine sends a fresh one later) */
            memset(&g_grants[i], 0, sizeof g_grants[i]);
        }
    lookup = g_lookup;
    pthread_mutex_unlock(&g_mu);
    put(&b, "{\"grants\":[");
    for (int i = 0; i < n; i++) {
        mesh_engine_t who;
        char hex[65], ip[16];
        int known = lookup ? lookup(got[i].key, &who) : 0;
        if (!known) memset(&who, 0, sizeof who);
        to_hex(got[i].key, hex);
        {
            const uint8_t *q = (const uint8_t *)&who.ip;
            snprintf(ip, sizeof ip, "%u.%u.%u.%u", q[0], q[1], q[2], q[3]);
        }
        if (i) put(&b, ",");
        put(&b, "{\"engine_id\":");
        putstr(&b, hex);
        put(&b, ",\"name\":");
        putstr(&b, who.name);
        put(&b, ",\"kind\":");
        putstr(&b, who.kind);
        put(&b, ",\"addr\":");
        if (who.ip) putstr(&b, ip);
        else put(&b, "null");
        if (who.play_port) putf(&b, ",\"port\":%lld", who.play_port);
        else put(&b, ",\"port\":null");
        put(&b, ",\"app_ws\":");
        if (who.app_ws[0]) putstr(&b, who.app_ws);
        else put(&b, "null");
        put(&b, ",\"grant\":");
        putstr(&b, got[i].grant);
        putf(&b, ",\"expires_s\":%lld}", (long long)((got[i].until_ms - now_ms + 999u) / 1000u));
    }
    put(&b, "]}");
    if (b.err) {
        /* never lost to a short buffer: put them back */
        pthread_mutex_lock(&g_mu);
        for (int i = 0; i < n; i++)
            for (size_t k = 0; k < sizeof g_grants / sizeof g_grants[0]; k++)
                if (!g_grants[k].used) {
                    g_grants[k] = got[i];
                    break;
                }
        pthread_mutex_unlock(&g_mu);
        return -1;
    }
    return (int)b.len;
}
