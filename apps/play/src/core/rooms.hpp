// PS5Upload Play - Play Rooms, the guest's side of the lists and the room (docs/ROOMS.md, the
// Play Rooms design): the hub's room list (GET /rooms), a friend's room lookup (GET
// /rooms/status), the host engine's preview (GET /api/join/preview, or the hub's relayed copy),
// the room push while playing ({"room": ...}), sorting and filtering the Join list, the expected
// round trip on a card, and the name rules. Pure logic: no sockets, no clock of its own.
// SPDX-License-Identifier: GPL-3.0-or-later
#pragma once
#include <cstddef>
#include <cstdint>

namespace play::rooms
{
// The rooms protocol this app speaks (1 = 12.6.x Play Together, 2 = 13.0 rooms). A host or a
// guest two versions apart still plays (the design's 10.5).
constexpr int kProto = 2;
constexpr int kProtoWindow = 2;

enum class Vis : std::uint8_t
{
    priv,
    friends,
    pub,
};
Vis vis_from(const char *s) noexcept; // "private" / "friends" / "public"
const char *vis_word(Vis v) noexcept;

// Where a card came from: the Join list's four groups, top to bottom.
enum class Group : std::uint8_t
{
    friends, // Your friends' rooms (friend passes, GET /rooms/status)
    shared,  // Shared with you (an invite pushed to this PS5 in the last 30 minutes)
    nearby,  // On this network (discovery's `together`)
    pub,     // Public rooms (the hub's list)
};

// The ping bars (the design's 6.1): 4 Excellent, 3 Good, 2 Playable, 1 Poor, 0 unknown / lost.
// The tier words are route.cpp's ("excellent" ... "down").
int bars_of(const char *tier) noexcept;

struct Card
{
    Group group = Group::pub;
    char room[24] = {};   // the lobby id on the hub ("" for a nearby game or a pushed invite)
    char name[40] = {};   // the room's name
    char host[40] = {};   // the host's display name
    char tag[8] = {};     // the host device's short tag ("4F2A")
    char game_title[72] = {};
    char game_id[12] = {}; // "PPSA01234"
    int proto = 1;
    char ver[16] = {};
    // capabilities the ad lists (the design's 10.5)
    bool cap_ps5app = false, cap_preview = false, cap_chat = false, cap_voice = false, cap_knock = false;
    int in = 0, max = 0; // players in / room size
    bool pw = false;
    bool locked = false; // Friends lookup only
    char vis[10] = {};   // "friends" / "public" when the list says
    char lang[8] = {};
    char region[16] = {}; // na-west na-east central-am south-am eu-west eu-east middle-east africa
                          // asia-south asia-east asia-se oceania (PROTOCOL.md "Rooms")
    char pk[100] = {}; // the room's key (sealed knocks)
    int est_ms = -1;   // the hub's estimate
    int age_s = 0;
    // filled in on this PS5
    int measured_ms = -1; // a real round trip from Details (a direct way)
    int history_ms = -1;  // the last games with this host (by tag)
    char addr[64] = {};   // a nearby game's engine
    std::uint16_t port = 0;
    char from[40] = {};   // Shared with you: who sent it
};

// The list the hub answers for GET /rooms, one page at a time.
struct List
{
    static constexpr int kMax = 48;
    Card cards[kMax];
    int n = 0;
    int next = -1; // the next page, -1: none
    int proto_min = 0, proto_latest = 0;
    char app_latest[16] = {};
    char country[4] = {};
    char region[16] = {}; // the viewer's, as the hub sees it
    bool off = false;     // the hub has rooms switched off
};
// Appends the page's rooms to `list` (keeps what is there); false on an unreadable answer.
bool parse_list(const char *text, std::size_t len, List &list) noexcept;
// GET /rooms/status: {"<lobby>": {"slots", "game", "pw", "locked", "pk"}}: updates the friends'
// cards whose room is in it; a card not in the answer is not open (removed). Returns how many stay.
// every_s: how often to ask again (the hub says; 300 in its cut-back mode).
int parse_status(const char *text, std::size_t len, Card *cards, int n, int *every_s = nullptr) noexcept;

// Sorts (the design's 7.0). L1/R1 cycle them on the PS5.
enum class Sort : std::uint8_t
{
    best,
    latency,
    game,
    players,
    space,
    newest,
    count_,
};
const char *sort_param(Sort s) noexcept; // the hub's ?sort= word
Sort sort_from(const char *s) noexcept;

struct Filter
{
    bool space = false;  // Has space
    bool nopw = false;   // No password
    bool myver = true;   // Works with my version (and takes PS5 app guests)
    bool near = false;   // Near me (expected round trip within Playable)
    char lang[8] = {};   // "" = any
    char game[72] = {};  // a title (or part of it) or a title id; "" = any
    bool any() const noexcept
    {
        return space || nopw || !myver || near || lang[0] || game[0];
    }
};
// Can this app join it at all (protocol window, PS5 app guests welcome)?
bool compatible(const Card &c) noexcept;
bool passes(const Card &c, const Filter &f) noexcept;
// Public cards only are sorted; the other groups keep their order (friends first, never paged away).
void sort(Card *cards, int n, Sort s) noexcept;
// The hub's query for a sort, a filter and a page.
std::size_t list_query(char *out, std::size_t cap, Sort s, const Filter &f, int page) noexcept;

// The round trip a card shows: measured, else the last games with that host, else the hub's
// estimate; -1 unknown. `measured` says which.
int expected_ms(const Card &c, bool *measured = nullptr) noexcept;
// The bars for a round trip alone (the tier lines 20/40/80 ms).
int bars_for_ms(int ms) noexcept;
bool full(const Card &c) noexcept;

// The host engine's preview (the design's 6.4), what Details shows.
struct Preview
{
    bool ok = false;
    int proto = 1;
    bool cap_chat = false, cap_voice = false, cap_ready = false, cap_group = false;
    char host[40] = {}, tag[8] = {}, ver[16] = {};
    int playing_min = -1;
    char room_name[40] = {}, vis[10] = {}, approval[12] = {}, chat[8] = {}, lang[8] = {}, region[16] = {};
    int size = 0, watchers = 0, asking = 0;
    bool pw = false;
    char game_id[12] = {}, game_title[72] = {}, game_players[12] = {};
    char res[12] = {}, codec[8] = {};
    int fps = 0, kbps = 0;
    bool hdr = false;
    int link_rtt = -1;
    char link_tier[12] = {};
    struct Player
    {
        char name[40] = {};
        int seat = 0;
        char kind[8] = {}; // host, ps5, web, app
        char tier[12] = {};
        int rtt_ms = -1;
    } players[4];
    int n_players = 0;
};
bool parse_preview(const char *text, std::size_t len, Preview &p) noexcept;

// The room as the host engine pushes it to each player in game (`{"room": {...}}`, 10.2), and
// the voice and mute messages beside it.
struct RoomState
{
    bool known = false;
    char name[40] = {};
    int size = 0;
    struct Player
    {
        int id = -1;          // the voice speaker id (the engine's), -1 not said
        char name[40] = {};
        std::uint32_t color = 0; // 0xRRGGBB, 0 not said
        int seat = 0;
        bool you = false;
        bool speaking = false;
        bool muted = false;   // host-muted voice (🔇 beside the name)
    } players[8];
    int n_players = 0;
    int you_rtt = -1, link_rtt = -1;
    char you_tier[12] = {}, link_tier[12] = {};
    char chat[8] = {};     // "off" / "quick" / "text" ("" = the host has no chat)
    char voice[8] = {};    // "off" / "ptt" / "open" ("" = the host has no voice)
    bool muted_voice = false, muted_chat = false;
    char paused[8] = {};   // voice paused: "poor" (the connection needs it for the game)
    // The engine's chat and voice (API.md "Play Rooms chat and voice"): {"comms"} and {"voice"}.
    bool comms_known = false;
    int notice_ver = 0;     // the notice version the engine wants accepted
    bool accepted = false;  // this person's acceptance reached the engine
    bool cap_chat = false, cap_voice = false;
    int you_sid = -1;
    bool mic_on = false, ptt = false, stepped = false;
    int kbps = 24, frame_ms = 20;
    struct Person
    {
        int sid = -1;
        char name[40] = {};
        int player = 0;
        bool host = false, muted = false, paused = false, mic = false, speaking = false;
    } people[8];
    int n_people = 0;
    // the last refusal ({"comms_refused": {what, why, notice?, retry_ms?}}); rev bumps on each
    char refused_what[8] = {}, refused_why[16] = {};
    int retry_ms = 0;
    std::uint32_t refused_rev = 0;
};
// Reads any of {"room"}, {"comms"}, {"voice"}, {"muted"}, {"comms_refused"} into `st` (the parts a
// message does not carry stay); false when the message is none of them.
bool parse_room_push(const char *text, std::size_t len, RoomState &st) noexcept;

// Names (the design's 3.5): 1-24 characters of letters, digits, spaces and basic punctuation;
// no links, '@', control or zero-width characters. The text is trimmed and spaces collapsed into
// out; false when nothing usable is left.
bool clean_name(const char *in, char *out, std::size_t cap) noexcept;
// A password typed for a room: 4-64 characters (anything printable).
bool password_ok(const char *pw) noexcept;

// The friend's room lookup query: ?r=a,b,c (at most 16 ids).
std::size_t status_query(char *out, std::size_t cap, const char *const *lobbies, int n) noexcept;
} // namespace play::rooms
