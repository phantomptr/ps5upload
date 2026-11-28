// PS5Upload Play - the app: screens, controller navigation, and the guest and host roles.
// SPDX-License-Identifier: GPL-3.0-or-later
#pragma once
#include "../core/chat.hpp"
#include "../core/friends.hpp"
#include "../core/invite.hpp"
#include "../core/rooms.hpp"
#include "../core/settings.hpp"
#include "../core/textkb.hpp"
#include "../core/voice.hpp"
#include "../net/net.hpp"
#include "../ui/canvas.hpp"
#include "../ui/strings.hpp"
#include "companion.hpp"
#include "helper.hpp"
#include "guest.hpp"
#include "rooms_net.hpp"
#include "waiting_room.hpp"

#include <atomic>
#include <cstdint>
#include <pthread.h>

namespace play
{
class App
{
  public:
    enum class Screen : std::uint8_t
    {
        home,
        pick,     // which computer
        way,      // code or nearby
        code,     // code keyboard
        address,  // address keypad
        joining,  // connecting / asking / result
        playing,
        host,
        settings,
        // Play Rooms (docs/ROOMS.md): rooms_ui.cpp, room_host.cpp
        rooms,    // the Join list: friends' rooms, shared with you, nearby, public rooms
        details,  // one room's Details
        filter,   // the Join list's filter sheet
        keyboard, // the on-screen keyboard (a password, a name, a chat line, a game)
        notice,   // the responsibility notice (once)
        thanks,   // Thanks & Support
        welcome,  // the first-run guide
        help,     // the controls
        confirm,  // an ending or loosening action, said in words first
        menu,     // the host's player menu / blocked list
    };

    App(const char *version, ui::Image bg, ui::Image logo) noexcept;
    void load_settings() noexcept;
    // Uses a language ("" = the PS5's own), as Settings does.
    void set_language(const char *code) noexcept;
    // One frame: input, state, drawing. `ui` is the full-screen picture, `layer` the overlay.
    void frame(std::uint64_t now_us, std::uint32_t *ui, std::uint32_t *layer) noexcept;
    // Commands from the dev file (`join ADDR PORT CODE`, `nearby ADDR PORT`, `host ADDR PORT`,
    // `pick`, `pickhost`, `pair`, `drop`, `leave`, `home`, `exit`).
    void command(const char *line) noexcept;

    // For screenshots on a computer: show a screen with made-up state.
    struct Fake
    {
        bool on = false;
        GuestView guest;
        CompanionView host;
        int found = 0;
        net::Answer answers[4];
        bool picture = false;
        float ps = 0, leave = 0;
        int helper_present = -1;
        helper::Rp rp;
        helper::Pin pin;
        WaitingRoom::View room; // the home screen's phone code
        // Play Rooms
        RoomsNet::View rooms;
        chat::Log chat;
        bool overlay = false; // the in-game Room menu open
        bool mic_on = false;
        int notice_accepted = -1; // -1: as the settings say
    };
    void fake(Screen s, const Fake &f) noexcept
    {
        fake_ = f;
        screen_ = s;
        dirty_ = true;
        ov_open_ = f.overlay;
        rv_ = f.rooms;
        mic_on_ = f.mic_on;
        if (f.notice_accepted >= 0)
            settings_.notice = f.notice_accepted;
    }
    // For screenshots: the keyboard, the confirm dialog, the welcome page, the host's tab.
    void fake_keyboard(const char *title, const char *text, bool secret) noexcept;
    void fake_confirm_close() noexcept;
    void fake_page(int welcome_page, int host_tab) noexcept
    {
        welcome_page_ = welcome_page;
        host_tab_ = host_tab;
    }
    void draw(std::uint32_t *ui, std::uint32_t *layer, std::uint64_t now_us, bool *show_ui, int *top,
              int *bottom) noexcept;

  private:
    // Navigation
    void go(Screen s) noexcept;
    void back() noexcept;
    void on_input(std::uint32_t pressed, std::uint32_t held, std::uint64_t now) noexcept;
    void input_home(std::uint32_t p) noexcept;
    void input_pick(std::uint32_t p) noexcept;
    void input_way(std::uint32_t p) noexcept;
    void input_code(std::uint32_t p) noexcept;
    void input_address(std::uint32_t p) noexcept;
    void input_joining(std::uint32_t p) noexcept;
    void input_host(std::uint32_t p) noexcept;
    void input_settings(std::uint32_t p) noexcept;

    // Actions
    void start_join(const char *code) noexcept;
    void start_host() noexcept;
    void stop_host() noexcept;
    void leave_game() noexcept;
    void save() noexcept;
    void apply_language() noexcept;
    int language_ = 0; // the language in use (ui::i18n index)
    void remember_current() noexcept;
    static void on_token(const char *addr, std::uint16_t port, const char *token, void *ctx) noexcept;

    // This console's helper (Remote Play readiness and PIN), watched while the host screen is up.
    struct HelperState
    {
        int present = -1; // -1 not asked yet, 0 not running, 1 running
        bool rp_known = false;
        helper::Rp rp;
        helper::Pin pin;
        char error[96] = {};
        std::uint64_t paired_until_us = 0; // "paired" shows this long
    };
    static void *helper_main(void *self) noexcept;
    HelperState helper_state() noexcept;
    pthread_t helper_thread_{};
    std::atomic<bool> helper_run_{false};
    pthread_mutex_t helper_lock_;
    HelperState helper_;
    HelperState helper_view_; // this frame's copy (the main thread's)
    std::uint64_t rp_confirm_until_ = 0;
    void refresh_helper_view() noexcept;

    // Discovery runs on its own thread while the pick screen is up.
    static void *discovery_main(void *self) noexcept;
    void start_discovery() noexcept;

    // ---- Play Rooms (docs/ROOMS.md) ----
    // rooms_ui.cpp: the Join list, Details, filter, keyboard, notice, Thanks, welcome, help
    struct JoinItem
    {
        enum Kind : std::uint8_t
        {
            card,
            more,    // the next page
            retry,   // the list didn't come: try again
            code,    // Enter a code
            address, // Type an address
        } kind = card;
        rooms::Card c;
    };
    int join_items(JoinItem *out, int cap) noexcept;
    void open_rooms() noexcept;
    void input_rooms(std::uint32_t p) noexcept;
    void input_details(std::uint32_t p) noexcept;
    void input_filter(std::uint32_t p) noexcept;
    void input_keyboard(std::uint32_t p, std::uint32_t held) noexcept;
    void input_notice(std::uint32_t p) noexcept;
    void input_thanks(std::uint32_t p) noexcept;
    void input_welcome(std::uint32_t p) noexcept;
    void input_help(std::uint32_t p) noexcept;
    void input_confirm(std::uint32_t p) noexcept;
    void input_menu(std::uint32_t p) noexcept;
    void draw_rooms(ui::Canvas &c) noexcept;
    void draw_card(ui::Canvas &c, int x, int y, int w, int h, bool sel, const rooms::Card &k) noexcept;
    void draw_details(ui::Canvas &c) noexcept;
    void draw_details_panel(ui::Canvas &c, int x, int y, int w, const rooms::Card &k, bool full) noexcept;
    void draw_filter(ui::Canvas &c) noexcept;
    void draw_keyboard(ui::Canvas &c) noexcept;
    void draw_notice(ui::Canvas &c) noexcept;
    void draw_thanks(ui::Canvas &c) noexcept;
    void draw_welcome(ui::Canvas &c) noexcept;
    void draw_help(ui::Canvas &c) noexcept;
    void draw_confirm(ui::Canvas &c) noexcept;
    void draw_menu(ui::Canvas &c) noexcept;
    // 4 bars for a tier (4 Excellent ... 1 Poor, 0 none); hollow = an estimate. Returns the width.
    static int draw_bars(ui::Canvas &c, int x, int y, int bars, bool hollow) noexcept;
    // Joining a card from the Join list: the notice, a password, then the knock.
    void choose_card(const rooms::Card &k) noexcept;
    void join_card(const rooms::Card &k) noexcept;
    // The keyboard for a purpose; done_keyboard() acts on what was typed.
    enum class Kb : std::uint8_t
    {
        password,
        name,
        chat,
        game,
        room_name,
        room_pw,
    };
    void open_keyboard(Kb why, const char *title, const char *text, int max_chars, bool secret) noexcept;
    void done_keyboard() noexcept;
    // What waits for the notice to be accepted.
    enum class Pending : std::uint8_t
    {
        none,
        join_card,
        details,
        mic,
        chat,
        make_public,
        chat_on,
    };
    // True when the notice is accepted (or not needed); else it is shown, `then` waits for it.
    bool need_notice(int need, Pending then) noexcept;
    void run_pending() noexcept;
    void load_supporters() noexcept;
    void load_device() noexcept;
    void save_resume(const GuestView &v) noexcept;
    void clear_resume() noexcept;
    // resume.txt: a game to go back to (host restarted, or this app was closed): kept 5 minutes.
    struct Resume
    {
        char addr[64] = {};
        std::uint16_t port = 0;
        char token[80] = {};
        char path[80] = {};
        char name[65] = {};
        std::uint64_t at = 0;
    } resume_;
    bool load_resume() noexcept;
    // room_host.cpp: the host's My room / Remote Play / This PS5 tabs
    bool owner_here(const CompanionDoc &d) const noexcept;
    void host_action_denied(const CompanionDoc &d) noexcept;
    void open_confirm_remove(const CompanionDoc::Guest &g) noexcept;
    void open_confirm_close(const CompanionDoc &d) noexcept;
    // room_overlay.cpp: the in-game Room menu, chat lines over the game, voice
    void overlay_frame(std::uint64_t now) noexcept;
    void input_overlay(std::uint32_t p) noexcept;
    int draw_overlay(ui::Canvas &l, const GuestView &v, std::uint64_t now) noexcept; // rows used from the top
    int draw_chat_lines(ui::Canvas &l, const GuestView &v, std::uint64_t now) noexcept; // rows used at the bottom
    void voice_tick(const GuestView &v) noexcept;
    const char *quick_text(int id) const noexcept;

    // Drawing
    void draw_frame(ui::Canvas &c, const char *title) noexcept;
    void draw_hints(ui::Canvas &c, const int *glyphs, const ui::S *labels, int n, int y = 990) noexcept;
    void draw_row(ui::Canvas &c, int x, int y, int w, int h, bool sel, const char *title, const char *sub,
                  const char *value = nullptr) noexcept;
    void draw_home(ui::Canvas &c) noexcept;
    void draw_pick(ui::Canvas &c) noexcept;
    void draw_way(ui::Canvas &c) noexcept;
    void draw_code(ui::Canvas &c) noexcept;
    void draw_address(ui::Canvas &c) noexcept;
    void draw_joining(ui::Canvas &c, const GuestView &v) noexcept;
    void draw_playing_ui(ui::Canvas &c, const GuestView &v) noexcept;
    int draw_playing_layer(ui::Canvas &c, const GuestView &v, std::uint64_t now, int *top) noexcept;
    void draw_host(ui::Canvas &c, const CompanionView &v) noexcept;
    void draw_settings(ui::Canvas &c) noexcept;
    const char *guest_message(const GuestView &v, char *buf, std::size_t cap) const noexcept;

    GuestView guest_view() noexcept;
    CompanionView host_view() noexcept;

    // Host screen rows (built from the status each frame)
    struct Row
    {
        enum Kind : std::uint8_t
        {
            pair,
            ask,
            open,
            new_code,
            close,
            guest,
            sw_block,
            sw_max,
            sw_seat,
            sw_invites,
            sw_awake,
            rp_enable,
            rp_pin,
            // Play Rooms
            room_vis,
            room_pw,
            room_ap_friends,
            room_ap_password,
            room_chat,
            room_voice,
            room_start_muted,
            room_lock,
            room_blocked,
            room_mute_all,
            room_name,
            tab_hint, // a line saying the room is closed / easier in PS5Upload
        } kind;
        long long id;
        int player;
    };
    int host_rows(const CompanionView &v, Row *rows, int cap) const noexcept;

    const char *version_;
    ui::Image bg_, logo_;
    Settings settings_;
    Guest *guest_ = nullptr;
    Companion *host_ = nullptr;
    Screen screen_ = Screen::home;
    Screen stack_[8];
    int depth_ = 0;
    int sel_ = 0;
    bool for_host_ = false; // the pick screen chooses the host's computer
    // Play Rooms state
    RoomsNet *rooms_net_ = nullptr;
    RoomsNet::View rv_;
    std::uint32_t rv_rev_ = 0;
    rooms::Sort sort_ = rooms::Sort::best;
    rooms::Filter filter_;
    char focus_key_[72] = {};
    std::uint64_t focus_since_ = 0;
    textkb::Keyboard kb_;
    Kb kb_for_ = Kb::password;
    char kb_title_[200] = {};
    std::uint64_t kb_typed_us_ = 0;
    Pending pending_ = Pending::none;
    rooms::Card pending_card_;
    rooms::Card join_card_; // the card being joined (for Try again and the password)
    bool join_from_card_ = false;
    struct Confirm
    {
        enum Act : std::uint8_t
        {
            close_room,
            remove,
            make_public,
            remove_pw,
        } act = close_room;
        int player = 0;
        char title[600] = {};
        bool from_rooms = false;
        bool block_room = false, block_always = false;
        int sel = 0;
    } confirm_;
    int menu_player_ = 0;
    long long menu_guest_ = 0;
    bool menu_blocked_ = false; // the menu lists the blocked people
    int host_tab_ = 0;          // 0 My room, 1 Remote Play, 2 This PS5
    Row::Kind host_sel_kind_ = Row::pair;
    long long host_sel_id_ = -1;
    int welcome_page_ = 0;
    // in game
    bool ov_open_ = false;
    int ov_sel_ = 0;
    bool mic_on_ = false;
    voice::Ptt ptt_;
    bool password_asked_ = false;
    bool notice_sent_ = false;        // {"notice": n} went on this join
    std::uint32_t refused_seen_ = 0;  // the last comms refusal shown
    std::uint64_t ov_note_until_ = 0; // a refusal's words over the game
    bool talk_held_ = false; // Create held (push-to-talk), from this frame's chords
    chat::Log ov_chat_;
    std::uint32_t ov_chat_rev_ = 0;
    char device_[32] = {};
    voice::Mixer mixer_;     // each person's volume and mute (this PS5 only)
    std::uint32_t resume_saved_ = 0; // the joined token kept on disk (its first 4 bytes)
    char join_name_[48] = {};
    // the host's computer: its engine id (pairings are kept by it), its own address (not a
    // Tailscale forward on this console), a helper grant for the next hello, this app's id
    char host_id_[72] = {};
    char host_addr_[64] = {};
    std::uint16_t host_port_ = 0;
    char host_grant_[72] = {};
    char app_id_[40] = {};
    // Host from this PS5: the right computer by itself (a helper grant, the engine streaming this
    // PS5, a paired one, the only one that knows it); false: the person picks.
    bool host_auto() noexcept;
    // Where Minion asked this app to open (helper /v1/intent), read once at start.
    void take_intent() noexcept; // the name a join uses ("" = the PS5 user's); "Pick another name" sets it
    // Thanks & Support: names from the bundled list (or the engine's newer one)
    char supporters_[4096] = {};
    const char *names_[160] = {};
    int n_names_ = 0, n_anonymous_ = 0, thanks_page_ = 0;
    char kofi_[96] = {}, thanks_url_[96] = {};
    char addr_[64] = {};
    std::uint16_t port_ = 19113;
    char engine_name_[65] = {};
    char code_[8] = {};
    char typed_[32] = {};
    char note_[512] = {}; // a short line under a screen (e.g. "That doesn't look like an address")
    std::uint64_t note_until_ = 0;
    bool dirty_ = true;
    std::uint64_t last_draw_ = 0, last_tick_ = 0, joined_at_ = 0, ps_note_until_ = 0;
    bool was_front_ = true;
    bool streaming_ = false;
    std::uint32_t last_doc_rev_ = 0;
    int last_phase_ = -1;
    // repeat for held directions
    std::uint32_t rep_bits_ = 0;
    std::uint64_t rep_next_ = 0;
    // discovery
    pthread_t disc_thread_{};
    std::atomic<bool> disc_run_{false};
    pthread_mutex_t disc_lock_;
    net::Answer found_[8];
    int n_found_ = 0;
    bool disc_done_once_ = false;
    Fake fake_;

    // ---- easy join (easy.cpp, docs/EASY_JOIN.md) ----
    struct EasyRow
    {
        enum Kind : std::uint8_t
        {
            invite, // a host sent an invite (the waiting room)
            nearby, // a game open on this network
            again,  // a remembered host: "Play again"
            room,   // a friend's room is open (Rooms › Your friends' rooms)
            resume, // back to the game this PS5 was in (the host restarted, or the app closed)
        } kind = nearby;
        rooms::Card card; // room
        char name[65] = {};
        char addr[64] = {};
        std::uint16_t port = 0;
        const Friend *friend_ = nullptr;
    };
    // Where a join goes, as remembered for Play again (not the helper's local forward).
    struct JoinTarget
    {
        char addr[64] = {};
        std::uint16_t port = 0;
        bool tailnet = false;
        char path[80] = {};
        char name[65] = {};
        // the internet invite this join came with (kept with the host for Play again)
        char room[24] = {};
        char fp[100] = {};
        char key[132] = {};
    };
    static constexpr int kEasyRows = 2; // at most, above the home screen's own rows
    void easy_init() noexcept;
    void easy_frame(std::uint64_t now) noexcept;
    int easy_rows(EasyRow *out, int cap) noexcept;
    bool easy_choose(const EasyRow &r) noexcept;
    void draw_easy_qr(ui::Canvas &c) noexcept;
    void draw_easy_row(ui::Canvas &c, int x, int y, int w, int h, bool sel, const EasyRow &r) noexcept;
    void save_friends() noexcept;
    JoinExtras default_extras() const noexcept;
    // Starts the guest. `ways`: the ways a handed join already has (the invite's or the remembered
    // host's address, and the bridge last); null: start()'s address. Discovery adds the engine's
    // other addresses either way, unless the join goes only through this home's PS5Upload.
    void start_join_how(const char *code, const JoinExtras &x, const route::Candidate *ways = nullptr,
                        int n_ways = 0) noexcept;
    // This home's PS5Upload for an internet invite (docs/ROUTES.md §6), from discovery.
    bool own_ps5upload(char *addr, std::size_t cap, std::uint16_t *port) noexcept;
    void join_invite(const invite::Invite &inv) noexcept;
    void join_again(const Friend &f) noexcept;
    Friends friends_;
    WaitingRoom room_;
    JoinExtras last_extras_;
    route::Candidate last_ways_[route::Selector::kMax]; // a handed join's ways, for Try again
    int n_last_ways_ = 0;
    JoinTarget target_;
    bool friend_saved_ = false;
    bool link_saved_ = false;
    invite::Invite invite_;
    bool has_invite_ = false;
    std::uint64_t invite_until_ = 0;
    bool room_paused_ = false; // the phone code ran its time: △ shows it again
    int last_room_state_ = -1;
    bool last_room_expired_ = false;
};
} // namespace play
