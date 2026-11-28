// PS5Upload Play - the PS5Upload helper's app API on this console (127.0.0.1:9181, loopback only).
// Optional: when the helper is not running (connection refused) the app works without it and
// names the PS5Upload screen to use instead. Contract: payload/include/app_api.h.
// SPDX-License-Identifier: GPL-3.0-or-later
#pragma once
#include <cstddef>
#include <cstdint>

namespace play::helper
{
constexpr std::uint16_t kPort = 9181;
void set_port(std::uint16_t port) noexcept; // tests

// GET /v1/rp: Remote Play readiness on this console.
struct Rp
{
    bool ready = false, service_on = false, user_on = false, needs_user_permission = false, signed_in = false,
         psn_activated = false;
};
// GET/POST /v1/rp/pin: pairing a device by PIN.
struct Pin
{
    char state[12] = {}; // idle, waiting, paired, failed, timeout
    char pin[16] = {};
    int seconds_left = 0;
    char err[32] = {};
};
// GET /v1/mesh: the engines connected to the helper.
struct Engine
{
    char name[65] = {};
    char kind[16] = {};
    char version[24] = {};
    char addr[16] = {};
    bool tailnet = false;
    std::uint16_t port = 0; // 0: not said (the UDP probe, or the default 19113)
    bool preferred = false; // the one the helper says the app should dial
};

bool parse_rp(const char *text, std::size_t len, Rp &out) noexcept;
bool parse_pin(const char *text, std::size_t len, Pin &out) noexcept;
// The engines, the preferred one first, consoles left out; returns how many.
int parse_mesh(const char *text, std::size_t len, Engine *out, int cap) noexcept;

// Calls (short timeouts; any thread). False / -1 when the helper is not there or said no
// (err gets its error token when it gave one).
bool rp(Rp &out) noexcept;
bool rp_enable(Rp &out, char *err, std::size_t cap) noexcept;
bool pin_request(Pin &out, char *err, std::size_t cap) noexcept;
bool pin_get(Pin &out) noexcept;
bool pin_cancel() noexcept;
int mesh(Engine *out, int cap) noexcept;
// Opens the helper's forward to a tailnet engine; *local_port is then dialled on 127.0.0.1.
bool forward(const char *peer, std::uint16_t port, std::uint16_t *local_port, char *err, std::size_t cap) noexcept;
} // namespace play::helper
