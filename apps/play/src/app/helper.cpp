// PS5Upload Play - the PS5Upload helper's app API on this console.
// SPDX-License-Identifier: GPL-3.0-or-later
#include "helper.hpp"

#include "../core/json.hpp"
#include "../net/net.hpp"

#include <cstdio>
#include <cstring>

namespace play::helper
{
namespace
{
std::uint16_t port_ = kPort;
constexpr unsigned kTimeoutMs = 1500;
constexpr std::size_t kOut = 18 * 1024;

void str(const json::Doc &d, int i, char *out, std::size_t cap) noexcept
{
    if (!d.str(i, out, cap))
        out[0] = 0;
}

// One call; the answer's body in out. The status, or -1 (not there).
int call(const char *method, const char *path, const char *body, char *out, std::size_t cap) noexcept
{
    return net::http_request(method, "127.0.0.1", port_, path, body, out, cap, kTimeoutMs);
}

void error_token(const char *body, char *err, std::size_t cap) noexcept
{
    if (!err || !cap)
        return;
    json::Token t[16];
    json::Doc d(t, 16);
    err[0] = 0;
    if (d.parse(body, std::strlen(body)))
        str(d, d.get(0, "error"), err, cap);
    if (!err[0])
        std::snprintf(err, cap, "%s", "unreachable");
}
} // namespace

void set_port(std::uint16_t port) noexcept
{
    port_ = port;
}

bool parse_rp(const char *text, std::size_t len, Rp &out) noexcept
{
    json::Token t[256];
    json::Doc d(t, 256);
    out = Rp{};
    if (!d.parse(text, len) || d.type(0) != json::Type::object || d.get(0, "ready") < 0)
        return false;
    out.ready = d.boolean(d.get(0, "ready"));
    out.service_on = d.boolean(d.get(0, "service_on"));
    out.user_on = d.boolean(d.get(0, "user_on"));
    out.needs_user_permission = d.boolean(d.get(0, "needs_user_permission"));
    out.signed_in = d.boolean(d.get(0, "signed_in"));
    out.psn_activated = d.boolean(d.get(0, "psn_activated"));
    return true;
}

bool parse_pin(const char *text, std::size_t len, Pin &out) noexcept
{
    json::Token t[48];
    json::Doc d(t, 48);
    out = Pin{};
    if (!d.parse(text, len) || d.type(0) != json::Type::object || d.get(0, "state") < 0)
        return false;
    str(d, d.get(0, "state"), out.state, sizeof out.state);
    str(d, d.get(0, "pin"), out.pin, sizeof out.pin);
    out.seconds_left = static_cast<int>(d.integer(d.get(0, "seconds_left")));
    str(d, d.get(0, "err"), out.err, sizeof out.err);
    return true;
}

int parse_mesh(const char *text, std::size_t len, Engine *out, int cap) noexcept
{
    static json::Token t[1024]; // the discovery thread's alone
    json::Doc d(t, 1024);
    if (!d.parse(text, len) || d.type(0) != json::Type::object)
        return 0;
    int n = 0;
    auto take = [&](int e, bool preferred) {
        if (d.type(e) != json::Type::object || n >= cap)
            return;
        Engine x;
        str(d, d.get(e, "name"), x.name, sizeof x.name);
        str(d, d.get(e, "kind"), x.kind, sizeof x.kind);
        str(d, d.get(e, "version"), x.version, sizeof x.version);
        str(d, d.get(e, "addr"), x.addr, sizeof x.addr);
        x.tailnet = d.boolean(d.get(e, "tailnet"));
        const double p = d.num(d.get(e, "port"), 0);
        x.port = p > 0 && p < 65536 ? static_cast<std::uint16_t>(p) : 0;
        x.preferred = preferred;
        str(d, d.get(e, "id"), x.id, sizeof x.id);
        // Consoles are not engines; an entry without an address can't be dialled.
        if (!x.addr[0] || !std::strcmp(x.kind, "ps5") || !std::strcmp(x.kind, "console"))
            return;
        for (int k = 0; k < n; ++k)
            if (!std::strcmp(out[k].addr, x.addr))
                return;
        out[n++] = x;
    };
    take(d.get(0, "engine"), true);
    const int online = d.get(0, "online");
    for (std::uint32_t i = 0; i < d.count(online); ++i)
        take(d.item(online, i), false);
    return n;
}

int parse_grants(const char *text, std::size_t len, Grant *out, int cap) noexcept
{
    json::Token t[256];
    json::Doc d(t, 256);
    if (!d.parse(text, len) || d.type(0) != json::Type::object)
        return 0;
    const int g = d.get(0, "grants");
    int n = 0;
    for (std::uint32_t i = 0; i < d.count(g) && n < cap; ++i)
    {
        const int e = d.item(g, i);
        Grant x;
        str(d, d.get(e, "engine_id"), x.engine_id, sizeof x.engine_id);
        str(d, d.get(e, "name"), x.name, sizeof x.name);
        str(d, d.get(e, "kind"), x.kind, sizeof x.kind);
        str(d, d.get(e, "addr"), x.addr, sizeof x.addr);
        str(d, d.get(e, "app_ws"), x.app_ws, sizeof x.app_ws);
        str(d, d.get(e, "grant"), x.grant, sizeof x.grant);
        const double p = d.num(d.get(e, "port"), 0);
        x.port = p > 0 && p < 65536 ? static_cast<std::uint16_t>(p) : 0;
        x.expires_s = static_cast<int>(d.num(d.get(e, "expires_s"), 0));
        if (x.grant[0] && x.addr[0] && x.port)
            out[n++] = x;
    }
    return n;
}

int api_version() noexcept
{
    char buf[256];
    const int st = call("GET", "/v1/version", nullptr, buf, sizeof buf);
    if (st != 200)
        return st < 0 ? -1 : 0;
    json::Token t[16];
    json::Doc d(t, 16);
    return d.parse(buf, std::strlen(buf)) ? static_cast<int>(d.num(d.get(0, "api"), 0)) : 0;
}

int grants(Grant *out, int cap) noexcept
{
    static char buf[4096];
    // one-time: each grant is gone from the helper once read
    return call("GET", "/v1/play/grants", nullptr, buf, sizeof buf) == 200 ? parse_grants(buf, std::strlen(buf), out, cap) : 0;
}

bool intent(char *out, std::size_t cap) noexcept
{
    char buf[256];
    out[0] = 0;
    if (call("GET", "/v1/intent", nullptr, buf, sizeof buf) != 200)
        return false;
    json::Token t[16];
    json::Doc d(t, 16);
    if (!d.parse(buf, std::strlen(buf)))
        return false;
    str(d, d.get(0, "intent"), out, cap);
    return out[0] != 0;
}

bool rp(Rp &out) noexcept
{
    static char buf[kOut];
    return call("GET", "/v1/rp", nullptr, buf, sizeof buf) == 200 && parse_rp(buf, std::strlen(buf), out);
}

bool rp_enable(Rp &out, char *err, std::size_t cap) noexcept
{
    static char buf[kOut];
    const int st = call("POST", "/v1/rp/enable", "{\"confirm\":true}", buf, sizeof buf);
    if (st == 200 && parse_rp(buf, std::strlen(buf), out))
        return true;
    error_token(buf, err, cap);
    return false;
}

bool pin_request(Pin &out, char *err, std::size_t cap) noexcept
{
    char buf[2048];
    const int st = call("POST", "/v1/rp/pin", "{}", buf, sizeof buf);
    if (st == 200 && parse_pin(buf, std::strlen(buf), out))
        return true;
    error_token(buf, err, cap);
    return false;
}

bool pin_get(Pin &out) noexcept
{
    char buf[2048];
    return call("GET", "/v1/rp/pin", nullptr, buf, sizeof buf) == 200 && parse_pin(buf, std::strlen(buf), out);
}

bool pin_cancel() noexcept
{
    char buf[512];
    return call("POST", "/v1/rp/pin/cancel", "{}", buf, sizeof buf) == 200;
}

int mesh(Engine *out, int cap) noexcept
{
    static char buf[kOut];
    if (call("GET", "/v1/mesh", nullptr, buf, sizeof buf) != 200)
        return -1;
    return parse_mesh(buf, std::strlen(buf), out, cap);
}

bool forward(const char *peer, std::uint16_t port, std::uint16_t *local_port, char *err, std::size_t cap) noexcept
{
    char body[96], buf[1024];
    std::snprintf(body, sizeof body, "{\"peer\":\"%s\",\"port\":%u}", peer, static_cast<unsigned>(port));
    const int st = call("POST", "/v1/tailnet/forward", body, buf, sizeof buf);
    if (st == 200)
    {
        json::Token t[16];
        json::Doc d(t, 16);
        const double lp = d.parse(buf, std::strlen(buf)) ? d.num(d.get(0, "local_port"), 0) : 0;
        if (lp > 0 && lp < 65536)
        {
            *local_port = static_cast<std::uint16_t>(lp);
            return true;
        }
    }
    error_token(buf, err, cap);
    return false;
}
} // namespace play::helper
