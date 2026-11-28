// PS5Upload Play - the app: screens, controller navigation, and the guest and host roles.
// SPDX-License-Identifier: GPL-3.0-or-later
#include "app.hpp"

#include "../../third_party/qrcodegen/qrcodegen.h"
#include "../core/proto.hpp"
#include "../ui/i18n.hpp"
#include "../ui/strings.hpp"
#include "log.hpp"
#include "platform.hpp"
#include "route_pick.hpp"
#include "route_ui.hpp"

#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <new>

namespace play
{
namespace
{
using ui::Canvas;
using ui::Font;
using ui::S;
using ui::tr;
namespace col = ui::color;
namespace pad = proto::pad;

constexpr int kLeft = 140;     // left margin of every screen
constexpr int kTop = 96;       // title line
constexpr int kContent = 240;  // first row
constexpr std::uint32_t kUp = pad::kUp, kDown = pad::kDown, kLeftB = pad::kLeft, kRight = pad::kRight;
constexpr std::uint32_t kDirs = kUp | kDown | kLeftB | kRight;

// Glyph ids for hint bars
enum : int
{
    G_CROSS = static_cast<int>(Canvas::Glyph::cross),
    G_CIRCLE = static_cast<int>(Canvas::Glyph::circle),
    G_SQUARE = static_cast<int>(Canvas::Glyph::square),
    G_TRIANGLE = static_cast<int>(Canvas::Glyph::triangle),
    G_OPTIONS = static_cast<int>(Canvas::Glyph::options),
};

const char *kPad[] = {"1", "2", "3", "4", "5", "6", "7", "8", "9", ".", "0", ":"};
constexpr int kPadKeys = 14; // 12 + delete + OK

struct PickItem
{
    enum Kind : std::uint8_t
    {
        found,
        used,
        type,
    } kind;
    char addr[64];
    std::uint16_t port;
    char name[65];
    const Host *host;
    bool tailnet;
};

int pick_items(const net::Answer *found, int n_found, const Settings &s, PickItem *out, int cap) noexcept
{
    int n = 0;
    for (int i = 0; i < n_found && n < cap - 1; ++i)
    {
        PickItem &p = out[n++];
        p = PickItem{};
        p.kind = PickItem::found;
        net::ip_text(found[i].ip_be, p.addr);
        p.port = found[i].info.port;
        p.tailnet = found[i].tailnet;
        std::snprintf(p.name, sizeof p.name, "%s", found[i].info.name[0] ? found[i].info.name : p.addr);
    }
    const Host *recent[Settings::kHosts];
    const int nr = s.recent(recent, Settings::kHosts);
    for (int i = 0; i < nr && n < cap - 1; ++i)
    {
        bool dup = false;
        for (int k = 0; k < n; ++k)
            dup = dup || (out[k].port == recent[i]->port && !std::strcmp(out[k].addr, recent[i]->addr));
        if (dup)
            continue;
        PickItem &p = out[n++];
        p = PickItem{};
        p.kind = PickItem::used;
        std::snprintf(p.addr, sizeof p.addr, "%s", recent[i]->addr);
        p.port = recent[i]->port;
        std::snprintf(p.name, sizeof p.name, "%s", recent[i]->name[0] ? recent[i]->name : recent[i]->addr);
        p.host = recent[i];
    }
    PickItem &t = out[n++];
    t = PickItem{};
    t.kind = PickItem::type;
    return n;
}

int clampi(int v, int lo, int hi) noexcept
{
    return v < lo ? lo : v > hi ? hi : v;
}
} // namespace

App::App(const char *version, ui::Image bg, ui::Image logo) noexcept : version_{version}, bg_{bg}, logo_{logo}
{
    pthread_mutex_init(&disc_lock_, nullptr);
    pthread_mutex_init(&helper_lock_, nullptr);
    std::size_t cap = 0;
    std::uint8_t *b = platform::message_buffer(0, &cap);
    static Guest guest(platform::sinks(), b, cap);
    guest_ = &guest;
    b = platform::message_buffer(1, &cap);
    static Companion host(b, cap);
    host_ = &host;
}

void App::load_settings() noexcept
{
    char path[160], text[4096];
    std::snprintf(path, sizeof path, "%s/settings.txt", platform::data_dir());
    const long n = platform::read_file(path, text, sizeof text - 1);
    if (n > 0)
        settings_.parse(text, static_cast<std::size_t>(n));
    platform::music(settings_.music, settings_.music_volume);
    platform::sounds(settings_.sounds);
    apply_language();
    easy_init();
}

void App::set_language(const char *code) noexcept
{
    std::snprintf(settings_.language, sizeof settings_.language, "%s", code);
    apply_language();
    dirty_ = true;
}

void App::apply_language() noexcept
{
    static bool names_loaded = false;
    std::size_t len = 0;
    if (!names_loaded)
    {
        const std::uint8_t *d = platform::asset("i18n/names.bin", &len);
        names_loaded = d && ui::i18n::load_names(d, len);
    }
    // The choice in Settings, else the console's own language, else English.
    int lang = ui::i18n::find(settings_.language);
    if (lang < 0)
        lang = ui::i18n::from_system_id(platform::system_language());
    language_ = lang;
    if (lang == 0)
    {
        ui::i18n::use_english();
        return;
    }
    char path[48];
    std::snprintf(path, sizeof path, "i18n/%s.bin", ui::i18n::code(lang));
    const std::uint8_t *d = platform::asset(path, &len);
    if (!d || !ui::i18n::load(d, len))
    {
        log("i18n: %s unreadable: English", path);
        ui::i18n::use_english();
        language_ = 0;
        return;
    }
    log("i18n: %s", ui::i18n::code(lang));
}

void App::save() noexcept
{
    char path[160], text[4096];
    std::snprintf(path, sizeof path, "%s/settings.txt", platform::data_dir());
    const std::size_t n = settings_.write(text, sizeof text);
    if (!platform::write_file(path, text, n))
        log("settings: could not save %s", path);
}

void App::on_token(const char *addr, std::uint16_t port, const char *token, void *ctx) noexcept
{
    auto *app = static_cast<App *>(ctx);
    // Called on the companion's thread; the settings are only written from here and the main
    // thread's save() reads them: a token is a short copy, and saving again is harmless.
    Host *h = app->settings_.remember(addr, port, nullptr);
    if (h)
        std::snprintf(h->token, sizeof h->token, "%s", token);
    app->save();
}

// ---- navigation -----------------------------------------------------------------------------
void App::go(Screen s) noexcept
{
    if (depth_ < 8)
        stack_[depth_++] = screen_;
    screen_ = s;
    sel_ = 0;
    dirty_ = true;
    if (s == Screen::pick)
        start_discovery();
}

void App::back() noexcept
{
    platform::blip(platform::Blip::back);
    if (screen_ == Screen::host)
        stop_host();
    // (discovery runs while the home or pick screen is up: easy_frame)
    if (depth_ > 0)
        screen_ = stack_[--depth_];
    else
        screen_ = Screen::home;
    sel_ = 0;
    dirty_ = true;
    if (screen_ == Screen::pick)
        start_discovery();
}

void App::start_discovery() noexcept
{
    if (disc_run_.exchange(true))
        return;
    if (disc_thread_)
        pthread_join(disc_thread_, nullptr);
    if (pthread_create(&disc_thread_, nullptr, discovery_main, this) != 0)
    {
        disc_thread_ = {};
        disc_run_ = false;
    }
}

void *App::discovery_main(void *self) noexcept
{
    auto *app = static_cast<App *>(self);
    while (app->disc_run_.load())
    {
        std::uint32_t uni[Settings::kHosts];
        int nu = 0;
        const Host *recent[Settings::kHosts];
        const int nr = app->settings_.recent(recent, Settings::kHosts);
        for (int i = 0; i < nr; ++i)
            if (net::resolve(recent[i]->addr, &uni[nu]))
                ++nu;
        // The helper (when it runs) knows the engines in its mesh, Tailscale ones too.
        helper::Engine eng[8];
        const int ne = helper::mesh(eng, 8);
        for (int i = 0; i < ne && nu < Settings::kHosts; ++i)
            if (!eng[i].tailnet && net::resolve(eng[i].addr, &uni[nu]))
                ++nu;
        net::Answer a[8];
        int n = net::discover(uni, nu, 1500, a, 8);
        for (int i = 0; i < ne && n < 8; ++i)
        {
            std::uint32_t ip = 0;
            if (!net::resolve(eng[i].addr, &ip))
                continue;
            bool known = false;
            for (int k = 0; k < n; ++k)
                known = known || a[k].ip_be == ip;
            if (known)
                continue;
            net::Answer x{};
            x.ip_be = ip;
            x.tailnet = eng[i].tailnet;
            x.info.port = eng[i].port ? eng[i].port : 19113;
            std::snprintf(x.info.name, sizeof x.info.name, "%s", eng[i].name);
            std::snprintf(x.info.version, sizeof x.info.version, "%s", eng[i].version);
            a[n++] = x;
        }
        pthread_mutex_lock(&app->disc_lock_);
        std::memcpy(app->found_, a, sizeof(net::Answer) * static_cast<std::size_t>(n));
        app->n_found_ = n;
        app->disc_done_once_ = true;
        app->dirty_ = true;
        pthread_mutex_unlock(&app->disc_lock_);
        for (int i = 0; i < 30 && app->disc_run_.load(); ++i)
            net::sleep_ms(100);
    }
    return nullptr;
}

void App::remember_current() noexcept
{
    if (Host *h = settings_.remember(addr_, port_, engine_name_[0] ? engine_name_ : nullptr))
        (void)h;
    save();
}

void App::start_join(const char *code) noexcept
{
    // Asks to be remembered (Play again) and names the waiting room (easy.cpp).
    // Every address discovery found for this engine is a way to it (docs/ROUTES.md), easy.cpp.
    start_join_how(code, default_extras());
}

void App::start_host() noexcept
{
    remember_current();
    const Host *h = settings_.find(addr_, port_);
    host_->stop();
    host_->start(addr_, port_, h ? h->token : "", version_, on_token, this);
    if (!helper_run_.exchange(true))
    {
        pthread_mutex_lock(&helper_lock_);
        helper_ = HelperState{};
        pthread_mutex_unlock(&helper_lock_);
        if (pthread_create(&helper_thread_, nullptr, helper_main, this) != 0)
        {
            helper_thread_ = {};
            helper_run_ = false;
        }
    }
    log("app: host channel to %s:%u (%s)", addr_, static_cast<unsigned>(port_), h && h->token[0] ? "paired" : "not paired");
}

void App::stop_host() noexcept
{
    if (host_->running())
        host_->stop();
    if (helper_run_.exchange(false) && helper_thread_)
    {
        pthread_join(helper_thread_, nullptr);
        helper_thread_ = {};
    }
}

void *App::helper_main(void *self) noexcept
{
    auto *app = static_cast<App *>(self);
    std::uint64_t next_rp = 0;
    while (app->helper_run_.load())
    {
        const std::uint64_t now = net::now_us();
        pthread_mutex_lock(&app->helper_lock_);
        const bool waiting = !std::strcmp(app->helper_.pin.state, "waiting");
        pthread_mutex_unlock(&app->helper_lock_);
        if (now >= next_rp)
        {
            next_rp = now + 3000000;
            helper::Rp rp;
            const bool ok = helper::rp(rp);
            pthread_mutex_lock(&app->helper_lock_);
            if (app->helper_.present == -1 || ok != (app->helper_.present == 1))
                log("helper: app API %s", ok ? "answers" : "not there (the app works without it)");
            app->helper_.present = ok ? 1 : 0;
            app->helper_.rp_known = ok;
            if (ok)
                app->helper_.rp = rp;
            app->dirty_ = true;
            pthread_mutex_unlock(&app->helper_lock_);
        }
        if (waiting)
        {
            // The PIN's state, once a second while it is up (paired, failed, timeout end it).
            helper::Pin pin;
            if (helper::pin_get(pin))
            {
                pthread_mutex_lock(&app->helper_lock_);
                if (!std::strcmp(pin.state, "paired"))
                    app->helper_.paired_until_us = net::now_us() + 10000000;
                app->helper_.pin = pin;
                app->dirty_ = true;
                pthread_mutex_unlock(&app->helper_lock_);
            }
        }
        for (int i = 0; i < 10 && app->helper_run_.load(); ++i)
            net::sleep_ms(100);
    }
    return nullptr;
}

void App::refresh_helper_view() noexcept
{
    if (fake_.on)
    {
        helper_view_ = HelperState{};
        helper_view_.present = fake_.helper_present;
        helper_view_.rp_known = fake_.helper_present == 1;
        helper_view_.rp = fake_.rp;
        helper_view_.pin = fake_.pin;
        return;
    }
    helper_view_ = helper_state();
    if (!std::strcmp(helper_view_.pin.state, "paired") && net::now_us() > helper_view_.paired_until_us)
        std::snprintf(helper_view_.pin.state, sizeof helper_view_.pin.state, "%s", "idle");
}

App::HelperState App::helper_state() noexcept
{
    pthread_mutex_lock(&helper_lock_);
    HelperState h = helper_;
    pthread_mutex_unlock(&helper_lock_);
    return h;
}

void App::leave_game() noexcept
{
    platform::stream_pad(nullptr);
    guest_->leave();
}

GuestView App::guest_view() noexcept
{
    return fake_.on ? fake_.guest : guest_->view();
}

CompanionView App::host_view() noexcept
{
    return fake_.on ? fake_.host : host_->view();
}

// ---- input --------------------------------------------------------------------------------
void App::on_input(std::uint32_t p, std::uint32_t held, std::uint64_t now) noexcept
{
    // Held directions repeat (after 400 ms, every 110 ms).
    const std::uint32_t dirs = held & kDirs;
    if (p & kDirs)
    {
        rep_bits_ = p & kDirs;
        rep_next_ = now + 400000;
    }
    else if (dirs && dirs == rep_bits_ && now >= rep_next_)
    {
        p |= dirs;
        rep_next_ = now + 110000;
    }
    else if (!dirs)
        rep_bits_ = 0;
    if (!p)
        return;
    dirty_ = true;
    if ((p & pad::kOptions) && screen_ != Screen::playing && screen_ != Screen::settings)
    {
        platform::blip(platform::Blip::choose);
        go(Screen::settings);
        return;
    }
    switch (screen_)
    {
    case Screen::home: input_home(p); break;
    case Screen::pick: input_pick(p); break;
    case Screen::way: input_way(p); break;
    case Screen::code: input_code(p); break;
    case Screen::address: input_address(p); break;
    case Screen::joining: input_joining(p); break;
    case Screen::playing: break; // the controller is the friend's game's
    case Screen::host: input_host(p); break;
    case Screen::settings: input_settings(p); break;
    }
}

void App::input_home(std::uint32_t p) noexcept
{
    // Games nearby, an invite and Play again first (easy.cpp), then the home screen's own rows.
    EasyRow easy[kEasyRows];
    const int ne = easy_rows(easy, kEasyRows);
    const int n = ne + 4;
    if (p & kUp)
        sel_ = (sel_ + n - 1) % n, platform::blip(platform::Blip::move);
    if (p & kDown)
        sel_ = (sel_ + 1) % n, platform::blip(platform::Blip::move);
    sel_ = clampi(sel_, 0, n - 1);
    if (p & pad::kTriangle)
    {
        // The phone code again after its time ran out.
        room_paused_ = false;
        room_.arm();
        platform::blip(platform::Blip::choose);
    }
    if ((p & pad::kCross) && sel_ < ne)
    {
        easy_choose(easy[sel_]);
        return;
    }
    if (p & pad::kCross)
    {
        platform::blip(platform::Blip::choose);
        switch (sel_ - ne)
        {
        case 0: for_host_ = false, go(Screen::pick); break;
        case 1: for_host_ = true, go(Screen::pick); break;
        case 2: go(Screen::settings); break;
        default:
            save();
            platform::exit_app();
        }
    }
}

void App::input_pick(std::uint32_t p) noexcept
{
    PickItem items[20];
    pthread_mutex_lock(&disc_lock_);
    const int n = pick_items(found_, n_found_, settings_, items, 20);
    pthread_mutex_unlock(&disc_lock_);
    if (p & pad::kCircle)
        return back();
    if (p & kUp)
        sel_ = (sel_ + n - 1) % n, platform::blip(platform::Blip::move);
    if (p & kDown)
        sel_ = (sel_ + 1) % n, platform::blip(platform::Blip::move);
    sel_ = clampi(sel_, 0, n - 1);
    const PickItem &it = items[sel_];
    if ((p & pad::kTriangle) && it.kind == PickItem::used)
    {
        settings_.forget(it.host);
        save();
        platform::blip(platform::Blip::back);
        return;
    }
    if (p & pad::kSquare)
    {
        // look again now
        disc_run_ = false;
        if (disc_thread_)
            pthread_join(disc_thread_, nullptr), disc_thread_ = {};
        start_discovery();
        return;
    }
    if (!(p & pad::kCross))
        return;
    platform::blip(platform::Blip::choose);
    if (it.kind == PickItem::type)
    {
        std::snprintf(typed_, sizeof typed_, "%s", "");
        go(Screen::address);
        return;
    }
    std::snprintf(addr_, sizeof addr_, "%s", it.addr);
    port_ = it.port;
    std::snprintf(engine_name_, sizeof engine_name_, "%s", it.kind == PickItem::found ? it.name : (it.host ? it.host->name : ""));
    // As Play again remembers it (the engine, not the helper's forward below).
    target_ = JoinTarget{};
    std::snprintf(target_.addr, sizeof target_.addr, "%s", it.addr);
    target_.port = it.port;
    target_.tailnet = it.kind == PickItem::found && it.tailnet;
    std::snprintf(target_.name, sizeof target_.name, "%s", engine_name_);
    if (it.kind == PickItem::found && it.tailnet)
    {
        // Over Tailscale the app dials the helper's forward on this console.
        std::uint16_t local = 0;
        char err[32];
        if (!helper::forward(it.addr, it.port, &local, err, sizeof err))
        {
            std::snprintf(note_, sizeof note_, tr(S::pick_tailnet_fail), it.name, err);
            note_until_ = net::now_us() + 6000000;
            platform::blip(platform::Blip::bad);
            return;
        }
        log("app: %s over Tailscale through the helper's port %u", it.name, static_cast<unsigned>(local));
        std::snprintf(addr_, sizeof addr_, "%s", "127.0.0.1");
        port_ = local;
    }
    disc_run_ = false;
    if (for_host_)
    {
        start_host();
        go(Screen::host);
    }
    else
        go(Screen::way);
}

void App::input_way(std::uint32_t p) noexcept
{
    if (p & pad::kCircle)
        return back();
    if (p & (kUp | kDown))
        sel_ ^= 1, platform::blip(platform::Blip::move);
    if (p & pad::kCross)
    {
        platform::blip(platform::Blip::choose);
        if (sel_ == 0)
        {
            code_[0] = 0;
            go(Screen::code);
        }
        else
            start_join("");
    }
}

void App::input_code(std::uint32_t p) noexcept
{
    // 31 letters and digits + delete (index 31) on 4 rows of 8; Join (32) below.
    constexpr int kCols = 8, kKeys = 32;
    if (p & pad::kCircle)
        return back();
    const std::size_t len = std::strlen(code_);
    auto del = [&] {
        if (len)
            code_[len - 1] = 0, platform::blip(platform::Blip::back);
    };
    if (p & pad::kSquare)
        del();
    if (p & kLeftB)
        sel_ = sel_ == kKeys ? sel_ : (sel_ % kCols == 0 ? sel_ + kCols - 1 : sel_ - 1), platform::blip(platform::Blip::move);
    if (p & kRight)
        sel_ = sel_ == kKeys ? sel_ : (sel_ % kCols == kCols - 1 ? sel_ - kCols + 1 : sel_ + 1), platform::blip(platform::Blip::move);
    if (p & kUp)
        sel_ = sel_ == kKeys ? kKeys - kCols + 3 : (sel_ < kCols ? kKeys : sel_ - kCols), platform::blip(platform::Blip::move);
    if (p & kDown)
        sel_ = sel_ == kKeys ? 3 : (sel_ + kCols >= kKeys ? kKeys : sel_ + kCols), platform::blip(platform::Blip::move);
    const bool full = len == static_cast<std::size_t>(proto::kCodeLen);
    if ((p & pad::kTriangle) || ((p & pad::kCross) && sel_ == kKeys))
    {
        if (full)
        {
            platform::blip(platform::Blip::choose);
            start_join(code_);
        }
        else
            platform::blip(platform::Blip::bad);
        return;
    }
    if (p & pad::kCross)
    {
        if (sel_ == kKeys - 1)
            del();
        else if (!full)
        {
            code_[len] = proto::kCodeAlphabet[sel_];
            code_[len + 1] = 0;
            platform::blip(platform::Blip::choose);
            if (len + 1 == static_cast<std::size_t>(proto::kCodeLen))
                sel_ = kKeys; // straight to Join
        }
        else
            platform::blip(platform::Blip::bad);
    }
}

void App::input_address(std::uint32_t p) noexcept
{
    if (p & pad::kCircle)
        return back();
    const std::size_t len = std::strlen(typed_);
    // 3 columns x 4 rows, then delete (12) and OK (13)
    if (p & kLeftB)
        sel_ = sel_ >= 12 ? (sel_ == 12 ? 13 : 12) : (sel_ % 3 == 0 ? sel_ + 2 : sel_ - 1);
    if (p & kRight)
        sel_ = sel_ >= 12 ? (sel_ == 12 ? 13 : 12) : (sel_ % 3 == 2 ? sel_ - 2 : sel_ + 1);
    if (p & kUp)
        sel_ = sel_ >= 12 ? 9 + (sel_ - 12) * 2 : (sel_ < 3 ? 12 : sel_ - 3);
    if (p & kDown)
        sel_ = sel_ >= 12 ? (sel_ - 12) * 2 : (sel_ + 3 >= 12 ? 12 + (sel_ % 3 == 2 ? 1 : 0) : sel_ + 3);
    if (p & kDirs)
        platform::blip(platform::Blip::move);
    auto del = [&] {
        if (len)
            typed_[len - 1] = 0;
    };
    if (p & pad::kSquare)
        del();
    if ((p & pad::kTriangle) || ((p & pad::kCross) && sel_ == 13))
    {
        char a[64];
        std::uint16_t port = 0;
        if (split_address(typed_, a, sizeof a, &port, 19113))
        {
            std::snprintf(addr_, sizeof addr_, "%s", a);
            port_ = port;
            engine_name_[0] = 0;
            target_ = JoinTarget{};
            std::snprintf(target_.addr, sizeof target_.addr, "%s", a);
            target_.port = port;
            platform::blip(platform::Blip::choose);
            // replace the keypad with what comes next
            screen_ = stack_[--depth_];
            if (for_host_)
            {
                start_host();
                go(Screen::host);
            }
            else
                go(Screen::way);
        }
        else
        {
            std::snprintf(note_, sizeof note_, "%s", tr(S::addr_bad));
            note_until_ = net::now_us() + 4000000;
            platform::blip(platform::Blip::bad);
        }
        return;
    }
    if (p & pad::kCross)
    {
        if (sel_ == 12)
            del();
        else if (len + 1 < sizeof typed_ && len < 21)
        {
            typed_[len] = kPad[sel_][0];
            typed_[len + 1] = 0;
        }
        platform::blip(platform::Blip::choose);
    }
}

void App::input_joining(std::uint32_t p) noexcept
{
    const GuestView v = guest_view();
    const bool over = v.phase == GuestView::refused || v.phase == GuestView::ended || v.phase == GuestView::failed;
    if (p & pad::kCircle)
    {
        if (!over)
            guest_->leave();
        // back to how they chose to join
        while (depth_ > 0 && screen_ != Screen::way && screen_ != Screen::code)
            screen_ = stack_[--depth_];
        if (screen_ != Screen::way && screen_ != Screen::code)
            screen_ = Screen::home;
        sel_ = 0;
        platform::blip(platform::Blip::back);
        return;
    }
    if ((p & pad::kCross) && over)
    {
        platform::blip(platform::Blip::choose);
        // the same ways again (a handed invite's, the bridge too)
        start_join_how(code_, last_extras_, n_last_ways_ ? last_ways_ : nullptr, n_last_ways_);
    }
}

int App::host_rows(const CompanionView &v, Row *rows, int cap) const noexcept
{
    int n = 0;
    auto add = [&](Row::Kind k, long long id = 0, int player = 0) {
        if (n < cap)
            rows[n++] = Row{k, id, player};
    };
    if (v.phase == CompanionView::unpaired || v.phase == CompanionView::pairing)
    {
        add(Row::pair);
        return n;
    }
    if (v.phase != CompanionView::paired)
        return 0;
    const CompanionDoc &d = v.doc;
    // Friends asking first; then this console's Remote Play, through the helper when it runs.
    for (int i = 0; i < d.n_asks; ++i)
        add(Row::ask, d.asks[i].id);
    const HelperState &hs = helper_view_;
    if (hs.present == 1 && hs.rp_known &&
        (!hs.rp.service_on || (hs.rp.needs_user_permission && !hs.rp.user_on)))
        add(Row::rp_enable);
    if (hs.present == 1)
        add(Row::rp_pin);
    if (!d.together_open)
        add(Row::open);
    else
    {
        add(Row::new_code);
        for (int i = 0; i < d.n_guests; ++i)
            add(Row::guest, d.guests[i].id, d.guests[i].player);
        add(Row::close);
    }
    add(Row::sw_block);
    add(Row::sw_max);
    add(Row::sw_seat);
    add(Row::sw_invites);
    add(Row::sw_awake);
    return n;
}

void App::input_host(std::uint32_t p) noexcept
{
    if (p & pad::kCircle)
        return back();
    const CompanionView v = host_view();
    refresh_helper_view();
    Row rows[24];
    const int n = host_rows(v, rows, 24);
    if (v.phase == CompanionView::failed && (p & pad::kCross))
    {
        start_host();
        return;
    }
    if (!n)
        return;
    if (p & kUp)
        sel_ = (sel_ + n - 1) % n, platform::blip(platform::Blip::move);
    if (p & kDown)
        sel_ = (sel_ + 1) % n, platform::blip(platform::Blip::move);
    sel_ = clampi(sel_, 0, n - 1);
    const Row &r = rows[sel_];
    const CompanionDoc &d = v.doc;
    const bool cross = p & pad::kCross, left = p & kLeftB, right = p & kRight;
    const bool change = cross || left || right;
    auto ok = [](bool sent) { platform::blip(sent ? platform::Blip::choose : platform::Blip::bad); };
    switch (r.kind)
    {
    case Row::pair:
        if (cross)
            ok(host_->pair());
        break;
    case Row::rp_enable:
        if (cross)
        {
            // A change to the console's settings: a second press says yes.
            const std::uint64_t now = net::now_us();
            if (now > rp_confirm_until_)
            {
                rp_confirm_until_ = now + 5000000;
                platform::blip(platform::Blip::choose);
                break;
            }
            rp_confirm_until_ = 0;
            helper::Rp rp;
            char err[32] = {};
            const bool done = helper::rp_enable(rp, err, sizeof err);
            log("helper: turn on Remote Play: %s", done ? "done" : err);
            pthread_mutex_lock(&helper_lock_);
            if (done)
                helper_.rp = rp, helper_.rp_known = true, helper_.error[0] = 0;
            else
                std::snprintf(helper_.error, sizeof helper_.error, tr(S::host_rp_enable_failed), err);
            pthread_mutex_unlock(&helper_lock_);
            ok(done);
        }
        break;
    case Row::rp_pin:
        if (cross)
        {
            helper::Pin pin;
            char err[32] = {};
            bool done;
            if (!std::strcmp(helper_view_.pin.state, "waiting"))
            {
                done = helper::pin_cancel();
                std::snprintf(pin.state, sizeof pin.state, "%s", "idle");
            }
            else
                done = helper::pin_request(pin, err, sizeof err);
            log("helper: Remote Play PIN %s: %s", pin.state, done ? "ok" : err);
            pthread_mutex_lock(&helper_lock_);
            if (done)
                helper_.pin = pin, helper_.error[0] = 0;
            else
                std::snprintf(helper_.error, sizeof helper_.error, tr(S::host_rp_pin_failed), err);
            pthread_mutex_unlock(&helper_lock_);
            ok(done);
        }
        break;
    case Row::ask:
        if (cross)
            ok(host_->decide(r.id, true));
        if (p & pad::kSquare)
            ok(host_->decide(r.id, false));
        break;
    case Row::open:
        if (cross)
            ok(host_->invite(false));
        break;
    case Row::new_code:
        if (cross)
            ok(host_->invite(true));
        break;
    case Row::close:
        if (cross)
            ok(host_->close_together());
        break;
    case Row::guest:
        if (p & pad::kTriangle)
            ok(host_->kick(r.player));
        break;
    case Row::sw_block:
        if (change)
            ok(host_->set_switch("block_system_buttons", d.block_system_buttons ? "false" : "true"));
        break;
    case Row::sw_max:
        if (change)
        {
            int m = d.max_players + (left ? -1 : 1);
            if (m < 1)
                m = d.max_guests;
            if (m > d.max_guests)
                m = 1;
            char b[8];
            std::snprintf(b, sizeof b, "%d", m);
            ok(host_->set_switch("max_players", b));
        }
        break;
    case Row::sw_seat:
        if (change)
            ok(host_->set_switch("stream_seat", d.stream_seat ? "false" : "true"));
        break;
    case Row::sw_invites:
        if (change)
            ok(host_->set_switch("invites", std::strcmp(d.invites, "wifi") ? "\"wifi\"" : "\"anywhere\""));
        break;
    case Row::sw_awake:
        if (change)
        {
            static const char *modes[] = {"off", "transfers", "always"};
            int at = 0;
            for (int i = 0; i < 3; ++i)
                if (!std::strcmp(d.keep_awake, modes[i]))
                    at = i;
            at = (at + (left ? 2 : 1)) % 3;
            ok(host_->keep_awake(modes[at]));
        }
        break;
    }
}

void App::input_settings(std::uint32_t p) noexcept
{
    constexpr int kRows = 6;
    if (p & (pad::kCircle | pad::kOptions))
    {
        save();
        return back();
    }
    if (p & kUp)
        sel_ = (sel_ + kRows - 1) % kRows, platform::blip(platform::Blip::move);
    if (p & kDown)
        sel_ = (sel_ + 1) % kRows, platform::blip(platform::Blip::move);
    const bool cross = p & pad::kCross, left = p & kLeftB, right = p & kRight;
    if (!(cross || left || right))
        return;
    switch (sel_)
    {
    case 0: settings_.music = !settings_.music; break;
    case 1: settings_.music_volume = clampi(settings_.music_volume + (left ? -5 : 5), 0, 100); break;
    case 2: settings_.sounds = !settings_.sounds; break;
    case 3: settings_.hints = !settings_.hints; break;
    case 4:
    {
        // Same as this PS5, then each language in turn
        const int n = ui::i18n::count();
        int at = ui::i18n::find(settings_.language); // -1: same as this PS5
        at = (at + 1 + (left ? n : 1)) % (n + 1) - 1;
        std::snprintf(settings_.language, sizeof settings_.language, "%s", at < 0 ? "" : ui::i18n::code(at));
        apply_language();
        break;
    }
    case 5:
        if (cross)
        {
            for (auto &h : settings_.hosts)
                h = Host{};
            friends_.forget_all();
            save_friends();
            std::snprintf(note_, sizeof note_, "%s", tr(S::set_forgot));
            note_until_ = net::now_us() + 3000000;
        }
        break;
    }
    platform::music(settings_.music && !streaming_, settings_.music_volume);
    platform::sounds(settings_.sounds);
    platform::blip(platform::Blip::choose);
    save();
}

// ---- the frame ----------------------------------------------------------------------------
void App::command(const char *line) noexcept
{
    // `verb addr port code`, split on spaces
    char verb[16] = {}, a[64] = {}, c[16] = {}, num[8] = {};
    char *fields[4] = {verb, a, num, c};
    const std::size_t caps[4] = {sizeof verb, sizeof a, sizeof num, sizeof c};
    int n = 0;
    for (const char *p = line; *p && n < 4;)
    {
        while (*p == ' ')
            ++p;
        if (!*p)
            break;
        std::size_t k = 0;
        while (*p && *p != ' ')
        {
            if (k + 1 < caps[n])
                fields[n][k++] = *p;
            ++p;
        }
        fields[n++][k] = 0;
    }
    const unsigned port = static_cast<unsigned>(std::strtoul(num, nullptr, 10));
    log("app: command %s", verb);
    if (!std::strcmp(verb, "exit"))
    {
        save();
        platform::exit_app();
    }
    if (!std::strcmp(verb, "leave"))
        leave_game();
    else if (!std::strcmp(verb, "drop"))
        guest_->drop_socket();
    else if (!std::strcmp(verb, "home"))
    {
        stop_host();
        screen_ = Screen::home;
        depth_ = 0;
    }
    else if (!std::strcmp(verb, "pair"))
        host_->pair();
    else if (!std::strcmp(verb, "pick") || !std::strcmp(verb, "pickhost"))
    {
        stop_host();
        depth_ = 0;
        screen_ = Screen::home;
        for_host_ = !std::strcmp(verb, "pickhost");
        go(Screen::pick);
    }
    else if (n >= 3 && (!std::strcmp(verb, "join") || !std::strcmp(verb, "nearby") || !std::strcmp(verb, "host")))
    {
        std::snprintf(addr_, sizeof addr_, "%s", a);
        port_ = static_cast<std::uint16_t>(port);
        engine_name_[0] = 0;
        target_ = JoinTarget{};
        std::snprintf(target_.addr, sizeof target_.addr, "%s", a);
        target_.port = port_;
        depth_ = 0;
        screen_ = Screen::home;
        if (!std::strcmp(verb, "host"))
        {
            for_host_ = true;
            start_host();
            go(Screen::host);
        }
        else
        {
            for_host_ = false;
            go(Screen::way);
            start_join(!std::strcmp(verb, "join") && n >= 4 ? c : "");
        }
    }
    dirty_ = true;
}

void App::frame(std::uint64_t now, std::uint32_t *ui_px, std::uint32_t *layer) noexcept
{
    const platform::Input in = platform::input();
    // Back in front after the home screen or the control center: sockets may be gone.
    if (in.in_front && !was_front_)
    {
        guest_->nudge();
        dirty_ = true;
    }
    was_front_ = in.in_front;
    if (in.in_front)
        on_input(in.pressed, in.held, now);
    easy_frame(now);

    // The guest's progress moves the screens.
    const GuestView gv = guest_view();
    if (static_cast<int>(gv.phase) != last_phase_)
    {
        last_phase_ = gv.phase;
        dirty_ = true;
        if (gv.phase == GuestView::joined && screen_ == Screen::joining)
        {
            go(Screen::playing);
            joined_at_ = now;
        }
        const bool live = gv.phase == GuestView::joined || gv.phase == GuestView::reconnecting;
        if (live && !streaming_)
        {
            streaming_ = true;
            platform::music(false, 0); // silent while streaming
            platform::stream_pad(guest_);
        }
        if (!live && streaming_ && gv.phase != GuestView::connecting)
        {
            streaming_ = false;
            platform::stream_pad(nullptr);
            platform::music(settings_.music, settings_.music_volume);
            if (screen_ == Screen::playing)
            {
                screen_ = Screen::joining; // the result, in plain words
                dirty_ = true;
            }
        }
    }
    if (streaming_)
    {
        const platform::Chords ch = platform::chords();
        if (ch.leave_now)
        {
            log("app: leave chord");
            leave_game();
        }
        if (ch.ps_sent)
            ps_note_until_ = now + 2500000;
    }
    // Keep the console awake while playing or hosting (every 10 s).
    if ((streaming_ || screen_ == Screen::host) && now - last_tick_ >= 10000000)
    {
        last_tick_ = now;
        platform::power_tick();
    }
    const CompanionView hv = screen_ == Screen::host ? host_view() : CompanionView{};
    if (screen_ == Screen::host && (hv.doc.rev != last_doc_rev_ || hv.phase == CompanionView::pairing))
    {
        last_doc_rev_ = hv.doc.rev;
        dirty_ = true;
    }
    // Animated or live screens redraw a few times a second; the rest only when something changed.
    const bool live_screen = screen_ == Screen::joining || screen_ == Screen::playing || screen_ == Screen::host ||
                             screen_ == Screen::pick;
    if (!dirty_ && !(live_screen && now - last_draw_ > 250000))
        return;
    bool show_ui = true;
    int top = 0, bottom = 0;
    draw(ui_px, layer, now, &show_ui, &top, &bottom);
    last_draw_ = now;
    dirty_ = false;
    if (screen_ == Screen::playing)
        platform::overlay(layer, top, bottom);
    if (show_ui)
        platform::present(ui_px);
}

// ---- drawing ------------------------------------------------------------------------------
void App::draw(std::uint32_t *ui_px, std::uint32_t *layer, std::uint64_t now, bool *show_ui, int *top,
               int *bottom) noexcept
{
    Canvas c(ui_px);
    *show_ui = true;
    switch (screen_)
    {
    case Screen::home: draw_home(c); break;
    case Screen::pick: draw_pick(c); break;
    case Screen::way: draw_way(c); break;
    case Screen::code: draw_code(c); break;
    case Screen::address: draw_address(c); break;
    case Screen::joining: draw_joining(c, guest_view()); break;
    case Screen::host: draw_host(c, host_view()); break;
    case Screen::settings: draw_settings(c); break;
    case Screen::playing:
    {
        const GuestView v = guest_view();
        Canvas l(layer);
        *bottom = draw_playing_layer(l, v, now, top);
        const bool picture = fake_.on ? fake_.picture : platform::video_showing();
        if (picture)
            *show_ui = false;
        else
            draw_playing_ui(c, v);
        break;
    }
    }
    if (note_until_ && now < note_until_ && screen_ != Screen::playing)
    {
        const int tw = Canvas::text_width(Font::body, note_);
        if (tw <= 1600)
        {
            const int w = tw + 60;
            c.round_rect((ui::kW - w) / 2, 900, w, 64, 32, col::panel_hi);
            c.text_center(Font::body, ui::kW / 2, 908, note_, col::text);
        }
        else
        {
            // A longer one (what an internet invite needs) wraps, the box growing upwards.
            const int lines = tw / 1300 + 1, lh = ui::font_height(Font::body) + 6;
            const int h = lines * lh + 28;
            c.round_rect((ui::kW - 1560) / 2, 964 - h, 1560, h, 32, col::panel_hi);
            c.text_wrap(Font::body, (ui::kW - 1500) / 2, 964 - h + 14, 1500, note_, col::text);
        }
    }
}

void App::draw_frame(Canvas &c, const char *title) noexcept
{
    c.copy_from(bg_);
    if (title)
        c.text(Font::title, kLeft, kTop, title, col::text, ui::kW - 2 * kLeft);
}

void App::draw_hints(Canvas &c, const int *glyphs, const S *labels, int n, int y) noexcept
{
    int x = kLeft;
    for (int i = 0; i < n; ++i)
    {
        x += c.button(static_cast<Canvas::Glyph>(glyphs[i]), x, y, 44) + 12;
        x += c.text(Font::small, x, y + 22 - ui::font_height(Font::small) / 2, tr(labels[i]), col::muted) + 44;
    }
}

void App::draw_row(Canvas &c, int x, int y, int w, int h, bool sel, const char *title, const char *sub,
                   const char *value) noexcept
{
    c.round_rect(x, y, w, h, 18, sel ? col::panel_hi : col::panel, sel ? 245 : 200);
    if (sel)
        c.round_frame(x, y, w, h, 18, 3, col::accent);
    const int th = ui::font_height(Font::body);
    const int ty = sub && *sub ? y + h / 2 - th + 6 : y + (h - th) / 2;
    const int vw = value ? Canvas::text_width(Font::body, value) : 0;
    // A right-to-left language mirrors the row: the value on the left, the words on the right.
    const bool rtl = ui::i18n::right_to_left();
    const int box = w - 64 - (vw ? vw + 40 : 0);
    const int tx = rtl && vw ? x + 32 + vw + 40 : x + 32;
    c.text(Font::body, tx, ty, title, col::text, box);
    if (sub && *sub)
        c.text(Font::small, tx, y + h / 2 + 8, sub, col::muted, box);
    if (value)
        c.text(Font::body, rtl ? x + 32 : x + w - 32 - vw, y + (h - th) / 2, value, sel ? col::accent : col::muted);
}

void App::draw_home(Canvas &c) noexcept
{
    draw_frame(c, nullptr);
    // Right: the phone code (the waiting room), else the logo (easy.cpp).
    draw_easy_qr(c);
    c.text(Font::big, kLeft, 120, tr(S::app_name), col::text);
    c.text(Font::body, kLeft + 4, 220, tr(S::tagline), col::muted, 1060);
    EasyRow easy[kEasyRows];
    const int ne = easy_rows(easy, kEasyRows);
    sel_ = clampi(sel_, 0, ne + 3);
    // More rows: a little tighter.
    const int top = ne ? 300 : 340, step = ne ? 108 : 140, h = ne ? 96 : 116;
    for (int i = 0; i < ne; ++i)
        draw_easy_row(c, kLeft, top + i * step, 1060, h, sel_ == i, easy[i]);
    const S titles[] = {S::home_join, S::home_host, S::home_settings, S::home_exit};
    const S subs[] = {S::home_join_sub, S::home_host_sub, S::home_settings_sub, S::app_name};
    for (int i = 0; i < 4; ++i)
        draw_row(c, kLeft, top + (ne + i) * step, 1060, h, sel_ == ne + i, tr(titles[i]), i < 3 ? tr(subs[i]) : nullptr);
    if (!ne)
        c.text(Font::small, kLeft, 920, version_, col::muted);
    const bool again = room_paused_ || (fake_.on ? fake_.room.expired : room_.view().expired);
    const int g[] = {G_CROSS, G_OPTIONS, G_TRIANGLE};
    const S l[] = {S::hint_choose, S::hint_settings, S::hint_phone};
    draw_hints(c, g, l, again ? 3 : 2);
}

void App::draw_pick(Canvas &c) noexcept
{
    draw_frame(c, tr(for_host_ ? S::pick_title_host : S::pick_title_guest));
    PickItem items[20];
    net::Answer found[8];
    int nf;
    bool done;
    pthread_mutex_lock(&disc_lock_);
    nf = fake_.on ? fake_.found : n_found_;
    std::memcpy(found, fake_.on ? fake_.answers : found_, sizeof(net::Answer) * static_cast<std::size_t>(nf > 8 ? 8 : nf));
    done = disc_done_once_ || fake_.on;
    pthread_mutex_unlock(&disc_lock_);
    const int n = pick_items(found, nf, settings_, items, 20);
    sel_ = clampi(sel_, 0, n - 1);
    int y = kContent;
    // At most 6 rows fit; keep the selected one in view.
    const int first = sel_ > 5 ? sel_ - 5 : 0;
    for (int i = first; i < n && i < first + 6; ++i)
    {
        const PickItem &it = items[i];
        char sub[128];
        if (it.kind == PickItem::type)
            draw_row(c, kLeft, y, 1200, 104, sel_ == i, tr(S::pick_type), tr(S::pick_type_sub));
        else
        {
            std::snprintf(sub, sizeof sub, "%s:%u · %s", it.addr, static_cast<unsigned>(it.port),
                          tr(it.kind != PickItem::found ? S::pick_used : it.tailnet ? S::pick_tailnet : S::pick_found));
            draw_row(c, kLeft, y, 1200, 104, sel_ == i, it.name, sub);
        }
        y += 120;
    }
    if (nf == 0)
        c.text_wrap(Font::small, kLeft, y + 20, 1200, tr(done ? S::pick_none : S::pick_looking), col::muted);
    const int g[] = {G_CROSS, G_CIRCLE, G_SQUARE, G_TRIANGLE};
    const S l[] = {S::hint_choose, S::hint_back, S::hint_refresh, S::hint_forget};
    draw_hints(c, g, l, items[sel_].kind == PickItem::used ? 4 : 3);
}

void App::draw_way(Canvas &c) noexcept
{
    char t[160];
    std::snprintf(t, sizeof t, tr(S::way_title), engine_name_[0] ? engine_name_ : addr_);
    draw_frame(c, t);
    draw_row(c, kLeft, kContent, 1100, 116, sel_ == 0, tr(S::way_code), tr(S::way_code_sub));
    draw_row(c, kLeft, kContent + 140, 1100, 116, sel_ == 1, tr(S::way_nearby), tr(S::way_nearby_sub));
    const int g[] = {G_CROSS, G_CIRCLE};
    const S l[] = {S::hint_choose, S::hint_back};
    draw_hints(c, g, l, 2);
}

void App::draw_code(Canvas &c) noexcept
{
    draw_frame(c, tr(S::code_title));
    c.text(Font::small, kLeft, kTop + 70, tr(S::code_help), col::muted);
    // the six boxes: ABC-DEF
    const int bw = 104, bh = 136, gap = 18;
    int x = kLeft;
    const std::size_t len = std::strlen(code_);
    for (int i = 0; i < proto::kCodeLen; ++i)
    {
        if (i == 3)
        {
            c.fill(x + 6, 240 + bh / 2 - 4, 36, 8, col::muted);
            x += 48;
        }
        const bool cur = static_cast<std::size_t>(i) == len;
        c.round_rect(x, 240, bw, bh, 16, col::panel, 230);
        c.round_frame(x, 240, bw, bh, 16, 3, cur ? col::accent : col::panel_hi);
        if (static_cast<std::size_t>(i) < len)
        {
            const char ch[2] = {code_[i], 0};
            c.text_center(Font::big, x + bw / 2, 240 + (bh - ui::font_height(Font::big)) / 2, ch, col::text);
        }
        x += bw + gap;
    }
    // the keyboard: 4 rows of 8, then Join
    const int kw = 112, kh = 92, kg = 14, ky = 440;
    for (int i = 0; i < 32; ++i)
    {
        const int kx = kLeft + (i % 8) * (kw + kg), yy = ky + (i / 8) * (kh + kg);
        const bool sel = sel_ == i;
        c.round_rect(kx, yy, kw, kh, 14, sel ? col::accent : col::panel, sel ? 255 : 220);
        if (i < 31)
        {
            const char ch[2] = {proto::kCodeAlphabet[i], 0};
            c.text_center(Font::title, kx + kw / 2, yy + (kh - ui::font_height(Font::title)) / 2, ch,
                          sel ? col::on_accent : col::text);
        }
        else
            c.text_center(Font::body, kx + kw / 2, yy + (kh - ui::font_height(Font::body)) / 2, "←",
                          sel ? col::on_accent : col::text);
    }
    const bool full = len == static_cast<std::size_t>(proto::kCodeLen);
    const int jy = ky + 4 * (kh + kg), jw = 8 * kw + 7 * kg;
    c.round_rect(kLeft, jy, jw, kh, 14, sel_ == 32 ? col::accent : full ? col::accent_dim : col::panel,
                 full || sel_ == 32 ? 255 : 160);
    c.text_center(Font::title, kLeft + jw / 2, jy + (kh - ui::font_height(Font::title)) / 2, tr(S::code_join),
                  sel_ == 32 ? col::on_accent : full ? col::text : col::muted);
    const int g[] = {G_CROSS, G_SQUARE, G_TRIANGLE, G_CIRCLE};
    const S l[] = {S::hint_type, S::hint_delete, S::hint_join, S::hint_back};
    draw_hints(c, g, l, 4);
}

void App::draw_address(Canvas &c) noexcept
{
    draw_frame(c, tr(S::addr_title));
    c.text(Font::small, kLeft, kTop + 70, tr(S::addr_help), col::muted);
    c.round_rect(kLeft, 240, 900, 110, 16, col::panel, 230);
    c.round_frame(kLeft, 240, 900, 110, 16, 3, col::accent);
    char shown[40];
    std::snprintf(shown, sizeof shown, "%s_", typed_);
    c.text(Font::big, kLeft + 30, 240 + (110 - ui::font_height(Font::big)) / 2, shown, col::text);
    const int kw = 180, kh = 92, kg = 16, ky = 400;
    for (int i = 0; i < kPadKeys; ++i)
    {
        int kx, yy, w = kw;
        const char *label;
        if (i < 12)
        {
            kx = kLeft + (i % 3) * (kw + kg);
            yy = ky + (i / 3) * (kh + kg);
            label = kPad[i];
        }
        else
        {
            w = (3 * kw + 2 * kg - kg) / 2;
            kx = kLeft + (i - 12) * (w + kg);
            yy = ky + 4 * (kh + kg);
            label = i == 12 ? "←" : tr(S::addr_ok);
        }
        const bool sel = sel_ == i;
        c.round_rect(kx, yy, w, kh, 14, sel ? col::accent : col::panel, sel ? 255 : 220);
        c.text_center(Font::title, kx + w / 2, yy + (kh - ui::font_height(Font::title)) / 2, label,
                      sel ? col::on_accent : col::text);
    }
    const int g[] = {G_CROSS, G_SQUARE, G_TRIANGLE, G_CIRCLE};
    const S l[] = {S::hint_type, S::hint_delete, S::hint_ok, S::hint_back};
    draw_hints(c, g, l, 4);
}

const char *App::guest_message(const GuestView &v, char *buf, std::size_t cap) const noexcept
{
    struct Why
    {
        const char *why;
        S s;
    };
    static const Why refused[] = {
        {"key", S::why_key},           {"expired", S::why_expired},       {"full", S::why_full},
        {"denied", S::why_denied},     {"no_answer", S::why_no_answer},   {"closed", S::why_closed},
        {"not_playing", S::why_not_playing}, {"old_helper", S::why_old_helper},
        {"unreachable", S::why_unreachable_ps5}, {"slow_down", S::why_slow_down},
        {"seat_taken", S::why_seat_taken}, {"hello", S::why_hello},
        {"friend", S::why_friend},
    };
    static const Why ended[] = {
        {"left", S::end_left},           {"kicked", S::end_kicked},     {"closed", S::end_closed},
        {"replaced", S::end_replaced},   {"host_stopped", S::end_host_stopped}, {"lost", S::end_lost},
        {"seat_taken", S::end_seat_taken},
    };
    const char *engine = engine_name_[0] ? engine_name_ : v.engine;
    switch (v.phase)
    {
    case GuestView::idle:
    case GuestView::connecting:
        std::snprintf(buf, cap, tr(S::st_connecting), engine);
        return buf;
    case GuestView::asking: return tr(S::st_asking);
    case GuestView::waiting: return tr(S::st_waiting);
    case GuestView::joined: return tr(S::st_picture);
    case GuestView::reconnecting: return tr(S::st_reconnecting);
    case GuestView::refused:
        for (const auto &w : refused)
            if (!std::strcmp(w.why, v.why))
                return tr(w.s);
        std::snprintf(buf, cap, tr(S::why_other), v.why);
        return buf;
    case GuestView::ended:
        for (const auto &w : ended)
            if (!std::strcmp(w.why, v.why))
                return tr(w.s);
        return tr(S::end_lost);
    case GuestView::failed:
        if (!std::strncmp(v.why, "bridge_", 7))
        {
            // Through this home's PS5Upload (docs/ROUTES.md §6): what went wrong, plainly.
            const char *b = v.why + 7;
            const char *at = v.bridge_at[0] ? v.bridge_at : v.engine;
            if (!std::strcmp(b, "old"))
                std::snprintf(buf, cap, tr(S::fail_bridge_old), at);
            else if (!std::strcmp(b, "unreachable"))
                std::snprintf(buf, cap, tr(S::fail_bridge_reach), at);
            else if (!std::strcmp(b, "hub"))
                return tr(S::fail_bridge_hub);
            else if (!std::strcmp(b, "no_room") || !std::strcmp(b, "mismatch"))
                return tr(S::fail_bridge_room);
            else if (!std::strcmp(b, "room_full"))
                return tr(S::why_full);
            else if (!std::strcmp(b, "no_direct"))
                return tr(S::fail_bridge_direct);
            else if (!std::strcmp(b, "busy"))
                return tr(S::fail_busy);
            else
                std::snprintf(buf, cap, tr(S::fail_bridge_other), b);
            return buf;
        }
        if (!std::strcmp(v.why, "busy"))
            return tr(S::fail_busy);
        if (!std::strcmp(v.why, "old_engine"))
            return tr(S::fail_old_engine);
        if (!std::strcmp(v.why, "closed"))
            return tr(S::fail_closed);
        std::snprintf(buf, cap, tr(S::fail_unreachable), v.engine);
        return buf;
    }
    return "";
}

void App::draw_joining(Canvas &c, const GuestView &v) noexcept
{
    draw_frame(c, nullptr);
    c.image(logo_, 1480, 330, 140);
    char buf[640]; // a message in UTF-8 with an address in it
    const char *msg = guest_message(v, buf, sizeof buf);
    const bool over = v.phase == GuestView::refused || v.phase == GuestView::ended || v.phase == GuestView::failed;
    const bool good_end = v.phase == GuestView::ended && !std::strcmp(v.why, "left");
    if (!over)
    {
        // a turning ring while waiting
        const float t = static_cast<float>(net::now_us() % 1200000) / 1200000.0f;
        c.ring(kLeft + 40, 420, 36, 8, col::accent, t, 0.3f);
        c.text_wrap(Font::title, kLeft + 110, 392, 1150, msg, col::text);
        if (v.phase == GuestView::asking)
            c.text(Font::body, kLeft + 110, 470, tr(S::st_asking_sub), col::muted);
    }
    else
    {
        c.circle(kLeft + 40, 420, 36, good_end ? col::good : col::warn);
        c.text_center(Font::title, kLeft + 40, 420 - ui::font_height(Font::title) / 2, good_end ? "✓" : "!", col::bg);
        c.text_wrap(Font::title, kLeft + 110, 392, 1150, msg, col::text);
    }
    if (over)
    {
        const int g[] = {G_CROSS, G_CIRCLE};
        const S l[] = {S::hint_retry, S::hint_back};
        draw_hints(c, g, l, 2);
    }
    else
    {
        const int g[] = {G_CIRCLE};
        const S l[] = {S::hint_back};
        draw_hints(c, g, l, 1);
    }
}

void App::draw_playing_ui(Canvas &c, const GuestView &v) noexcept
{
    // No picture (yet): the same words the overlay has, on the app's own background.
    draw_frame(c, nullptr);
    char buf[256];
    const char *msg = v.phase == GuestView::reconnecting ? tr(S::st_reconnecting) : tr(S::st_picture);
    const float t = static_cast<float>(net::now_us() % 1200000) / 1200000.0f;
    c.ring(kLeft + 40, 420, 36, 8, col::accent, t, 0.3f);
    c.text(Font::title, kLeft + 110, 392, msg, col::text);
    std::snprintf(buf, sizeof buf, tr(S::st_joined), v.player, v.host[0] ? v.host : tr(S::st_your_friend));
    c.text(Font::body, kLeft + 110, 470, buf, col::muted);
    if (!std::strcmp(v.codec_warning, "hevc") || !std::strcmp(v.codec_warning, "main10"))
        c.text_wrap(Font::body, kLeft + 110, 540, 1300,
                    tr(!std::strcmp(v.codec_warning, "hevc") ? S::play_codec_hevc : S::play_codec_main10), col::warn);
    c.text(Font::small, kLeft, 900, tr(S::play_hint_ps), col::muted);
    c.text(Font::small, kLeft, 940, tr(S::play_hint_leave), col::muted);
}

int App::draw_playing_layer(Canvas &l, const GuestView &v, std::uint64_t now, int *top) noexcept
{
    // Only the two bands are cleared and drawn: the rest of the layer is never read.
    constexpr int kTopBand = 120, kBottomBand = 150;
    const platform::Chords ch = fake_.on ? platform::Chords{fake_.ps, fake_.leave, false, false} : platform::chords();
    std::memset(l.pixels(), 0, sizeof(std::uint32_t) * ui::kW * kTopBand);
    std::memset(l.pixels() + ui::kW * (ui::kH - kBottomBand), 0, sizeof(std::uint32_t) * ui::kW * kBottomBand);
    *top = 0;
    int bottom = 0;
    // Top: what the person should know now.
    char buf[256];
    const char *banner = nullptr;
    std::uint32_t tone = col::panel;
    if (v.phase == GuestView::reconnecting)
        banner = tr(S::st_reconnecting), tone = col::accent_dim;
    else if (now < ps_note_until_)
        banner = tr(S::play_ps_sent);
    else if (v.move_first)
    {
        if (v.host_user[0])
            std::snprintf(buf, sizeof buf, tr(S::play_move_first), v.host_user), banner = buf;
        else
            banner = tr(S::play_move_first_plain);
        tone = col::accent_dim;
    }
    else if (v.took_host)
        banner = tr(S::play_took_host);
    else if (joined_at_ && now - joined_at_ < 6000000)
    {
        if (v.plays_as[0])
            std::snprintf(buf, sizeof buf, tr(S::play_plays_as), v.plays_as);
        else
            std::snprintf(buf, sizeof buf, tr(S::st_joined), v.player, v.host[0] ? v.host : tr(S::st_your_friend));
        banner = buf;
    }
    if (banner)
    {
        const int w = ui::Canvas::text_width(Font::body, banner) + 64;
        const int x = (ui::kW - w) / 2;
        // the layer holds straight alpha: draw the panel's pixels with alpha, then the text on it
        for (int y = 28; y < 28 + 64; ++y)
            for (int xx = x; xx < x + w; ++xx)
                l.pixels()[y * ui::kW + xx] = (tone & 0xffffffu) | (220u << 24);
        l.text(Font::body, x + 32, 28 + (64 - ui::font_height(Font::body)) / 2, banner, col::text);
        *top = kTopBand;
    }
    // Top right: the way to the host and how good it is (docs/ROUTES.md).
    if (route_ui::draw_chip(l, v, now, joined_at_))
        *top = kTopBand;
    // Bottom: the controls hint (first seconds), or the hold ring while a chord is held.
    const bool hold = ch.ps > 0 || ch.leave > 0;
    const bool hints = settings_.hints && joined_at_ && now - joined_at_ < 10000000;
    if (hold || hints)
    {
        const int y0 = ui::kH - kBottomBand + 30;
        const char *line1 = hold ? tr(ch.leave > 0 ? S::play_hold_leave : S::play_hold_ps) : tr(S::play_hint_ps);
        const char *line2 = hold ? nullptr : tr(S::play_hint_leave);
        const int w = 1100;
        const int x = (ui::kW - w) / 2;
        for (int y = y0; y < y0 + 96; ++y)
            for (int xx = x; xx < x + w; ++xx)
                l.pixels()[y * ui::kW + xx] = (col::panel & 0xffffffu) | (215u << 24);
        if (hold)
        {
            const float p = ch.leave > 0 ? ch.leave : ch.ps;
            l.ring(x + 56, y0 + 48, 30, 8, col::panel_hi);
            l.ring(x + 56, y0 + 48, 30, 8, ch.leave > 0 ? col::bad : col::accent, 0, p);
            // make the ring's pixels opaque in the layer
            for (int y = y0 + 14; y < y0 + 84; ++y)
                for (int xx = x + 22; xx < x + 92; ++xx)
                    l.pixels()[y * ui::kW + xx] |= 0xff000000u;
            l.text(Font::body, x + 110, y0 + (96 - ui::font_height(Font::body)) / 2, line1, col::text);
        }
        else
        {
            l.text(Font::small, x + 30, y0 + 14, line1, col::text);
            l.text(Font::small, x + 30, y0 + 52, line2, col::text);
        }
        bottom = kBottomBand;
    }
    return bottom;
}

void App::draw_host(Canvas &c, const CompanionView &v) noexcept
{
    refresh_helper_view();
    draw_frame(c, tr(S::home_host));
    char buf[320];
    const CompanionDoc &d = v.doc;
    const char *name = d.engine_name[0] ? d.engine_name : engine_name_[0] ? engine_name_ : addr_;
    c.text(Font::small, kLeft, kTop + 66, name, col::muted);
    Row rows[24];
    const int n = host_rows(v, rows, 24);
    sel_ = n ? clampi(sel_, 0, n - 1) : 0;
    if (v.phase == CompanionView::connecting || v.phase == CompanionView::idle || v.phase == CompanionView::retrying)
    {
        const float t = static_cast<float>(net::now_us() % 1200000) / 1200000.0f;
        c.ring(kLeft + 40, 420, 36, 8, col::accent, t, 0.3f);
        c.text(Font::title, kLeft + 110, 392, tr(v.phase == CompanionView::retrying ? S::host_retrying : S::host_connecting), col::text);
    }
    else if (v.phase == CompanionView::failed)
    {
        if (!std::strcmp(v.why, "old_engine"))
            c.text_wrap(Font::title, kLeft, 300, 1500, tr(S::host_old_engine), col::text);
        else
        {
            std::snprintf(buf, sizeof buf, tr(S::fail_unreachable), addr_);
            c.text_wrap(Font::title, kLeft, 300, 1500, !std::strcmp(v.why, "busy") ? tr(S::fail_busy) : buf, col::text);
        }
        const int g[] = {G_CROSS, G_CIRCLE};
        const S l[] = {S::hint_retry, S::hint_back};
        draw_hints(c, g, l, 2);
        return;
    }
    else if (v.phase == CompanionView::unpaired || v.phase == CompanionView::pairing)
    {
        c.text(Font::title, kLeft, 260, tr(S::host_pair_title), col::text);
        int y = 330;
        if (v.auth[0])
            y += c.text_wrap(Font::body, kLeft, y, 1500, tr(S::host_auth_dropped), col::warn) + 10;
        y += c.text_wrap(Font::body, kLeft, y, 1500, tr(S::host_pair_text), col::muted) + 30;
        if (v.phase == CompanionView::pairing && v.pair_left_s > 0)
        {
            c.round_rect(kLeft, y, 760, 200, 24, col::panel, 235);
            c.text_center(Font::huge, kLeft + 380, y + (200 - ui::font_height(Font::huge)) / 2, v.pair_display, col::text);
            y += 230;
            y += c.text_wrap(Font::body, kLeft, y, 1500, tr(S::host_pair_code_text), col::text) + 10;
            std::snprintf(buf, sizeof buf, tr(S::host_pair_left), v.pair_left_s / 60, v.pair_left_s % 60);
            c.text(Font::small, kLeft, y, buf, col::muted);
        }
        else
        {
            if (v.phase == CompanionView::pairing)
                y += c.text_wrap(Font::body, kLeft, y, 1500, tr(S::host_pair_gone), col::warn) + 10;
            draw_row(c, kLeft, y, 800, 104, true, tr(S::host_pair_button), nullptr);
        }
        const int g[] = {G_CROSS, G_CIRCLE};
        const S l[] = {S::hint_choose, S::hint_back};
        draw_hints(c, g, l, 2);
        return;
    }
    if (v.phase != CompanionView::paired)
        return;

    // Left: the session and what is around it, then the rows.
    int y = 210;
    if (d.playing)
        std::snprintf(buf, sizeof buf, tr(S::host_playing),
                      !std::strcmp(d.codec, "h264") ? "H.264" : !std::strcmp(d.codec, "h265_hdr") ? "HEVC HDR" : "HEVC",
                      (d.bitrate_kbps + 500) / 1000);
    c.circle(kLeft + 10, y + 18, 9, d.playing ? col::good : col::muted);
    c.text(Font::small, kLeft + 34, y + 4, d.playing ? buf : tr(S::host_idle), d.playing ? col::text : col::muted, 1060);
    y += 44;
    const HelperState &hs = helper_view_;
    if (hs.present == 1 && hs.rp_known && !hs.rp.ready)
    {
        // What this console's helper says is missing, in order.
        const S why = !hs.rp.service_on                                    ? S::host_rp_off_here
                      : hs.rp.needs_user_permission && !hs.rp.user_on      ? S::host_rp_user_here
                      : !hs.rp.psn_activated                               ? S::host_rp_account
                                                                           : S::host_rp_setup;
        c.circle(kLeft + 10, y + 18, 9, col::warn);
        c.text(Font::small, kLeft + 34, y + 4, tr(why), col::warn, 1060);
    }
    else
    {
        const bool good = d.rp_paired || (hs.present == 1 && hs.rp_known && hs.rp.ready);
        if (d.rp_paired)
            std::snprintf(buf, sizeof buf, tr(S::host_rp_ready), d.engine_name[0] ? d.engine_name : "PS5Upload");
        else
            std::snprintf(buf, sizeof buf, "%s", tr(good ? S::host_rp_on_here : S::host_rp_setup));
        c.circle(kLeft + 10, y + 18, 9, good ? col::good : col::warn);
        c.text(Font::small, kLeft + 34, y + 4, buf, good ? col::text : col::warn, 1060);
    }
    y += 44;
    // The helper: what this app saw on 127.0.0.1, else what the engine heard.
    const int helper_on = hs.present >= 0 ? hs.present : d.helper_present;
    if (helper_on >= 0)
    {
        c.circle(kLeft + 10, y + 18, 9, helper_on ? col::good : col::warn);
        c.text(Font::small, kLeft + 34, y + 4, tr(helper_on ? S::host_helper_on : S::host_helper_off),
               helper_on ? col::text : col::warn, 1060);
        y += 44;
    }
    if (hs.error[0])
    {
        c.text(Font::small, kLeft + 34, y + 4, hs.error, col::warn, 1060);
        y += 44;
    }
    y += 16;
    const int row_h = 76, row_w = 1100;
    // Seven rows fit above the note; keep the selected one in view.
    const int first = sel_ > 6 ? sel_ - 6 : 0;
    for (int i = first; i < n && i < first + 7; ++i)
    {
        const Row &r = rows[i];
        const bool s = sel_ == i;
        char title[160];
        const char *value = nullptr;
        switch (r.kind)
        {
        case Row::ask:
            for (int k = 0; k < d.n_asks; ++k)
                if (d.asks[k].id == r.id)
                    std::snprintf(title, sizeof title, tr(!std::strcmp(d.asks[k].client_kind, "ps5") ? S::host_asks_ps5 : S::host_asks),
                                  d.asks[k].name[0] ? d.asks[k].name : tr(S::host_friend));
            break;
        case Row::open: std::snprintf(title, sizeof title, "%s", tr(S::host_together_open)); break;
        case Row::new_code: std::snprintf(title, sizeof title, "%s", tr(S::host_new_code)); break;
        case Row::close: std::snprintf(title, sizeof title, "%s", tr(S::host_close)); break;
        case Row::guest:
            for (int k = 0; k < d.n_guests; ++k)
                if (d.guests[k].id == r.id)
                {
                    const char *gn = d.guests[k].name[0] ? d.guests[k].name : tr(S::host_friend);
                    if (d.guests[k].rtt_ms >= 0)
                        std::snprintf(title, sizeof title, tr(S::host_guest_rtt), d.guests[k].player, gn, d.guests[k].rtt_ms);
                    else
                        std::snprintf(title, sizeof title, tr(S::host_guest), d.guests[k].player, gn);
                    // how they are connected (docs/ROUTES.md): "Tailscale · Good"
                    static char way_buf[96];
                    route_ui::guest_line(d.guests[k].route, d.guests[k].tier, way_buf, sizeof way_buf);
                    if (way_buf[0])
                        value = way_buf;
                }
            break;
        case Row::sw_block:
            std::snprintf(title, sizeof title, "%s", tr(S::host_sw_block));
            value = tr(d.block_system_buttons ? S::on : S::off);
            break;
        case Row::sw_max:
        {
            std::snprintf(title, sizeof title, "%s", tr(S::host_sw_max));
            static char m[8];
            std::snprintf(m, sizeof m, "%d", d.max_players);
            value = m;
            break;
        }
        case Row::sw_seat:
            std::snprintf(title, sizeof title, "%s", tr(S::host_sw_seat));
            value = tr(d.stream_seat ? S::on : S::off);
            break;
        case Row::sw_invites:
            std::snprintf(title, sizeof title, "%s", tr(S::host_sw_invites));
            value = tr(!std::strcmp(d.invites, "wifi") ? S::wifi_only : S::anywhere);
            break;
        case Row::sw_awake:
            std::snprintf(title, sizeof title, "%s", tr(S::host_sw_awake));
            value = tr(!std::strcmp(d.keep_awake, "always") ? S::awake_always
                       : !std::strcmp(d.keep_awake, "transfers") ? S::awake_transfers
                                                                 : S::awake_off);
            break;
        case Row::pair: title[0] = 0; break;
        case Row::rp_enable:
            std::snprintf(title, sizeof title, "%s",
                          tr(net::now_us() < rp_confirm_until_ ? S::host_rp_enable_confirm : S::host_rp_enable));
            break;
        case Row::rp_pin:
            std::snprintf(title, sizeof title, "%s",
                          tr(!std::strcmp(hs.pin.state, "waiting") ? S::host_rp_pin_stop : S::host_rp_pin));
            break;
        }
        draw_row(c, kLeft, y, row_w, row_h - 10, s, title, nullptr, value);
        if (r.kind == Row::ask)
            c.circle(kLeft + 14, y + (row_h - 10) / 2, 7, col::warn);
        y += row_h;
    }
    // Right: the Remote Play PIN while it is up, else Play Together's invite (code + QR).
    const int rx = 1300;
    const bool pin_up = !std::strcmp(hs.pin.state, "waiting") || !std::strcmp(hs.pin.state, "paired");
    if (pin_up)
    {
        c.text(Font::title, rx, 210, tr(S::host_rp_pin_title), col::text);
        if (!std::strcmp(hs.pin.state, "paired"))
            c.text_wrap(Font::body, rx, 290, 540, tr(S::host_rp_pin_paired), col::good);
        else
        {
            char shown[20];
            const std::size_t pl = std::strlen(hs.pin.pin);
            // 1234 5678: easier to type
            if (pl == 8)
                std::snprintf(shown, sizeof shown, "%.4s %.4s", hs.pin.pin, hs.pin.pin + 4);
            else
                std::snprintf(shown, sizeof shown, "%s", hs.pin.pin);
            c.text(Font::big, rx, 290, shown, col::text);
            std::snprintf(buf, sizeof buf, tr(S::host_rp_pin_text), hs.pin.seconds_left / 60, hs.pin.seconds_left % 60);
            c.text_wrap(Font::small, rx, 390, 540, buf, col::muted);
        }
    }
    else
        c.text(Font::title, rx, 210, tr(S::host_together), col::text);
    if (pin_up)
        ; // the PIN has the right side
    else if (!d.together_open)
        c.text_wrap(Font::body, rx, 280, 520, tr(d.playing ? S::host_together_closed : S::host_together_wait), col::muted);
    else if (d.has_invite)
    {
        c.text(Font::small, rx, 280, tr(S::host_code), col::muted);
        c.text(Font::big, rx, 310, d.code_display, col::text);
        std::snprintf(buf, sizeof buf, tr(S::host_code_left), (d.invite_left_s + 59) / 60);
        c.text(Font::small, rx, 400, buf, col::muted);
        if (d.url[0])
        {
            static std::uint8_t qr[qrcodegen_BUFFER_LEN_MAX], tmp[qrcodegen_BUFFER_LEN_MAX];
            if (qrcodegen_encodeText(d.url, tmp, qr, qrcodegen_Ecc_MEDIUM, 1, 20, qrcodegen_Mask_AUTO, true))
            {
                const int size = qrcodegen_getSize(qr);
                const int module = (420 / (size + 4)) > 2 ? 420 / (size + 4) : 2;
                c.qr(qr, rx, 450, module);
                c.text(Font::small, rx, 450 + (size + 4) * module + 14,
                       tr(!std::strcmp(d.works, "anywhere") ? S::host_works_anywhere : S::host_works_wifi), col::muted);
            }
        }
    }
    if (!d.n_guests && d.together_open)
        c.text(Font::small, rx, 960 - 40, tr(S::host_no_guests), col::muted);
    // what to press for the selected row
    const Row *r = n ? &rows[sel_] : nullptr;
    if (r && r->kind == Row::ask)
    {
        const int g[] = {G_CROSS, G_SQUARE, G_CIRCLE};
        const S l[] = {S::hint_allow, S::hint_deny, S::hint_back};
        draw_hints(c, g, l, 3);
    }
    else if (r && r->kind == Row::guest)
    {
        const int g[] = {G_TRIANGLE, G_CIRCLE};
        const S l[] = {S::hint_remove, S::hint_back};
        draw_hints(c, g, l, 2);
    }
    else if (r && r->kind >= Row::sw_block && r->kind <= Row::sw_awake)
    {
        const int g[] = {G_CROSS, G_CIRCLE};
        const S l[] = {S::hint_change, S::hint_back};
        draw_hints(c, g, l, 2);
    }
    else
    {
        const int g[] = {G_CROSS, G_CIRCLE};
        const S l[] = {S::hint_choose, S::hint_back};
        draw_hints(c, g, l, 2);
    }
    c.text(Font::small, kLeft, 940, tr(S::host_game_note), col::muted);
}

void App::draw_settings(Canvas &c) noexcept
{
    draw_frame(c, tr(S::set_title));
    char vol[16];
    std::snprintf(vol, sizeof vol, "%d%%", settings_.music_volume);
    draw_row(c, kLeft, kContent, 1100, 90, sel_ == 0, tr(S::set_music), nullptr, tr(settings_.music ? S::on : S::off));
    draw_row(c, kLeft, kContent + 106, 1100, 90, sel_ == 1, tr(S::set_volume), nullptr, vol);
    draw_row(c, kLeft, kContent + 212, 1100, 90, sel_ == 2, tr(S::set_sounds), nullptr, tr(settings_.sounds ? S::on : S::off));
    draw_row(c, kLeft, kContent + 318, 1100, 90, sel_ == 3, tr(S::set_hints), nullptr, tr(settings_.hints ? S::on : S::off));
    const char *lang = ui::i18n::find(settings_.language) < 0 ? tr(S::lang_auto) : ui::i18n::name(language_);
    draw_row(c, kLeft, kContent + 424, 1100, 90, sel_ == 4, tr(S::set_language), nullptr, lang);
    draw_row(c, kLeft, kContent + 530, 1100, 90, sel_ == 5, tr(S::set_forget), nullptr);
    // About, on the right
    const int ax = 1320, aw = 520;
    int y = kContent;
    c.text(Font::body, ax, y, tr(S::set_about), col::text);
    y += 50;
    char buf[96];
    std::snprintf(buf, sizeof buf, tr(S::about_version), version_);
    c.text(Font::small, ax, y, buf, col::muted);
    y += 46;
    y += c.text_wrap(Font::small, ax, y, aw, tr(S::about_music), col::muted, 4) + 10;
    y += c.text_wrap(Font::small, ax, y, aw, tr(S::about_credits), col::muted, 4) + 10;
    c.text_wrap(Font::small, ax, y, aw, tr(S::about_license), col::muted, 4);
    const int g[] = {G_CROSS, G_CIRCLE};
    const S l[] = {S::hint_change, S::hint_back};
    draw_hints(c, g, l, 2);
}
} // namespace play
