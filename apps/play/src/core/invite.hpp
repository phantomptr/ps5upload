// PS5Upload Play - easy join (docs/EASY_JOIN.md): Play Together invite links handed to the app
// (the phone's "Play on my PS5", a host's "Send invite"), the hand-off message on the hub, and the
// waiting room's ids. Pure code: no sockets, tested on a computer.
// SPDX-License-Identifier: GPL-3.0-or-later
#pragma once
#include <cstddef>
#include <cstdint>

namespace play::invite
{
// One way to reach the host's engine directly: the home network's address, or a Tailscale one
// (100.64.0.0/10, reached through the helper's forward).
struct Route
{
    char addr[16] = {}; // dotted IPv4
    std::uint16_t port = 0;
    bool tailnet = false;
    char path[80] = {}; // the guest socket's path ("/api/join/ws", or under the engine's prefix)
    // Where the app dials it (the helper's forward on 127.0.0.1 for a Tailscale one); empty: addr.
    char dial_addr[16] = {};
    std::uint16_t dial_port = 0;
};

struct Invite
{
    char key[132] = {};   // the invite key (k= / i=): it only lets a guest ask
    char name[48] = {};   // the host's name, when said
    Route routes[6];
    int n_routes = 0;
    bool internet = false; // the link is an internet invite (room, fp, hub)
    char nonce[40] = {};   // the hand-off's `t` (the phone that scanned this TV's code)
};

// A Play Together link: `http://<engine>:<port>[/prefix]/join[/]#i=<key>[&n=<name>]`, or the one
// link for everywhere `https://…/join/#room=…&k=<key>&fp=…&hub=…[&lan=<page>][&ts=<page>][&n=…]`.
// Adds its direct routes (only private or Tailscale IPv4 addresses on http) to `inv` and sets the
// key (a link with another key than one already read is refused). False: not a Play Together link.
bool add_link(const char *link, Invite &inv) noexcept;

// The hand-off's data (a hub signal from a guest of the app's room):
// {"invite": {"link", "links"?: [...], "name"?, "t"?}}. False when it is not one, or has no key.
bool parse_handoff(const char *text, std::size_t len, Invite &inv) noexcept;

// Private (10/8, 172.16/12, 192.168/16) or Tailscale (100.64/10) IPv4: where the app goes directly.
bool private_v4(const char *addr, bool *tailnet) noexcept;

// ---- the waiting room ------------------------------------------------------------------------
// The hub's bound room id for a secret (hub.rs room_for_secret): base64url of
// SHA-256("ps5upload-hub room\0" + secret), 21 characters, then 'h'. out: 23 bytes.
void room_for_secret(const char *secret, char out[23]) noexcept;
// base64url without padding of n bytes into out (terminated); returns its length.
std::size_t b64url(const std::uint8_t *in, std::size_t n, char *out, std::size_t cap) noexcept;
// The QR code's link: `<page>#ps5=<room>&t=<nonce>&hub=<hub host>`.
std::size_t qr_link(char *out, std::size_t cap, const char *page, const char *room, const char *nonce,
                    const char *hub_host) noexcept;
// "wss://hub.example/ws" -> "hub.example" (with a port when it has one); false when unreadable.
bool hub_host(const char *url, char *out, std::size_t cap) noexcept;
} // namespace play::invite
