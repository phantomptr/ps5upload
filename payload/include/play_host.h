/* Play Rooms on this console (13.0.0): which engine hosts a room here and who plays (the
 * "hosted-by" marker), and PS5Upload Play's one-time pairing grants.
 *
 * Both come from engines over AVA1 and are kept in memory only (a helper restart forgets them; the
 * engine sends them again within its refresh):
 *
 *   play.room  (203)  {"room": R | null[, "ttl_s": N]} -> {"ok":true,"rooms":<how many are held>}
 *       The caller's room, replaced whole on every call, null clears it. Kept ttl_s seconds (10..600,
 *       default PLAY_ROOM_TTL_DEFAULT_S): the engine sends it again when it changes and at least every
 *       ttl_s / 3, so the room of an engine that went away (closed, crashed, network gone) disappears
 *       by itself. R (every field optional except name; unknown fields are ignored; strings cut to
 *       their bound at a character boundary, control characters dropped):
 *         "name"         the room's name (1..PLAY_ROOM_NAME_MAX bytes)
 *         "visibility"   "private" | "friends" | "public" (anything else: "private")
 *         "locked"       true when it has a password
 *         "game"         {"name": "...", "title_id": "PPSA01234"} | null
 *         "size"         most players (guests) it takes, 1..8
 *         "players"      [{"name": "...", "ping_ms": N | null, "seat": 2..8 | null, "bars": 0..4 | null,
 *                         "via": "browser|ps5app|phone|desktop|..."}] (bars: the rooms design's 6.1
 *                         tier, 4 Excellent .. 1 Poor, 0 no reading; null: shown from ping_ms),
 *                        at most PLAY_ROOM_PLAYERS (more are counted in "playing" only)
 *         "playing"      how many play now (default: the players listed)
 *         "asks"         how many wait for the host's answer
 *         "chat", "voice" true when on
 *         "hosted_from"  the host device's name as people know it (default: the name this console
 *                        lists the calling device under, Settings › Devices)
 *       Refused: {"ok":false,"err":"bad_request"} (not an object, no name) or "rooms_full" (more than
 *       PLAY_HOST_ENGINES engines hold one: the oldest stays, a new one is refused).
 *
 *   play.grant (204)  {"grant": "<64 hex>"[, "ttl_s": N]} | {"grant": null} -> {"ok":true,"expires_s":N}
 *       A one-time pairing grant for PS5Upload Play on this console (integration design F1): the
 *       engine that holds an AVA1 session with this helper proves the console, so the app needs no
 *       code. One per engine (a new one replaces the old), kept ttl_s seconds (30..600, default
 *       PLAY_GRANT_TTL_DEFAULT_S), handed out once: the app's read takes it (play_grants_take_json).
 *       The engine accepts the grant only from a PS5Upload Play hello on this console and only for
 *       its own engine id; the helper never shows it anywhere else (not Minion, not /v1/status).
 *
 * Readers:
 *   play_room_json      {"room": V | null, "rooms": [V, ...]} where V = the fields above as kept,
 *                       plus "engine_id" (64 hex), "kind" (the device's kind), "online" (its AVA1
 *                       session is live), "age_s" (since the engine last sent it). "room" is the one
 *                       heard from last. Minion's GET /api/room, the app API's /v1/room and the
 *                       "room" of /v1/status.
 *   play_grants_take_json {"grants": [{"engine_id","name","kind","addr","port","app_ws","grant",
 *                       "expires_s"}]}: every live grant, each removed as it is handed out. The app
 *                       API's GET/POST /v1/play/grants only (loopback).
 *
 * Pure (no sockets, no Sony calls): the device names come in through a lookup (mesh_engine_info on
 * the console, a fake in payload/tests/play_host_selftest.c). Thread-safe: one lock inside. */
#ifndef PS5UPLOAD_PLAY_HOST_H
#define PS5UPLOAD_PLAY_HOST_H

#include <stddef.h>
#include <stdint.h>

#include "mesh.h"

#define PLAY_HOST_ENGINES 4
#define PLAY_ROOM_NAME_MAX 96      /* bytes, UTF-8 */
#define PLAY_ROOM_GAME_MAX 128
#define PLAY_ROOM_FROM_MAX 64
#define PLAY_ROOM_PLAYER_NAME_MAX 48
#define PLAY_ROOM_PLAYERS 8
#define PLAY_ROOM_TTL_DEFAULT_S 90u
#define PLAY_ROOM_TTL_MIN_S 10u
#define PLAY_ROOM_TTL_MAX_S 600u
#define PLAY_GRANT_TTL_DEFAULT_S 300u
#define PLAY_GRANT_TTL_MIN_S 30u
#define PLAY_GRANT_TTL_MAX_S 600u
#define PLAY_HOST_REQ_MAX 8192u    /* a play.room / play.grant body */
#define PLAY_HOST_OUT_MAX 12288u   /* play_room_json's answer at most (4 full rooms) */

/* Who a key is (mesh_engine_info on the console). NULL: no names, "kind" "" and no address. */
typedef int (*play_host_lookup_fn)(const uint8_t key[32], mesh_engine_t *out);
void play_host_set_lookup(play_host_lookup_fn fn);

/* Forgets every room and grant (tests; a helper takeover). */
void play_host_reset(void);

/* The methods. now_ms is CLOCK_MONOTONIC in ms; `caller` the AVA1 peer's key. Each writes a JSON
 * answer into out and returns its length, -1 only when cap cannot hold it. */
int play_room_set_json(const uint8_t caller[32], const char *body, size_t n, uint64_t now_ms, char *out, size_t cap);
int play_grant_set_json(const uint8_t caller[32], const char *body, size_t n, uint64_t now_ms, char *out, size_t cap);

/* The readers (see above). */
int play_room_json(uint64_t now_ms, char *out, size_t cap);
int play_grants_take_json(uint64_t now_ms, char *out, size_t cap);

/* How many rooms are held now, and the players of the newest one (Minion's confirmations). */
int play_room_active(uint64_t now_ms, int *playing);

/* Where PS5Upload Play should open next (Minion's "Join a friend" opens the app at its join screen):
 * "join", "host" or "rooms", kept PLAY_INTENT_TTL_S and taken once by the app (GET /v1/intent ->
 * {"intent":"join"|null}). 0, or -1 for another word. */
#define PLAY_INTENT_TTL_S 60u
int play_intent_set(const char *intent, uint64_t now_ms);
int play_intent_take_json(uint64_t now_ms, char *out, size_t cap);

#endif
