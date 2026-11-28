// PS5Upload Play - the app: screens, controller navigation, and the guest and host roles.
// SPDX-License-Identifier: GPL-3.0-or-later
#pragma once
#include "../core/friends.hpp"
#include "../core/invite.hpp"
#include "../core/settings.hpp"
#include "../net/net.hpp"
#include "../ui/canvas.hpp"
#include "../ui/strings.hpp"
#include "companion.hpp"
#include "helper.hpp"
#include "guest.hpp"
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
    };
    void fake(Screen s, const Fake &f) noexcept
    {
        fake_ = f;
        screen_ = s;
        dirty_ = true;
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
        } kind = nearby;
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
