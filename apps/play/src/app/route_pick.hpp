// PS5Upload Play - the ways to one engine, from what discovery found (docs/ROUTES.md).
// SPDX-License-Identifier: GPL-3.0-or-later
#pragma once
#include "../core/friends.hpp"
#include "../core/invite.hpp"
#include "../net/net.hpp"
#include "../net/route.hpp"

namespace play
{
// Every address discovery found for the engine the person picked (the same engine id, or the
// same name when an answer has no id: the helper's mesh says no id), each as a way: a Tailscale
// one through the helper's forward, the rest dialled directly. The picked address itself too
// (a remembered or typed one), unless it is the helper's forward on 127.0.0.1. Returns how many.
int route_candidates(const net::Answer *found, int n_found, const char *picked_name, const char *picked_id,
                     const char *picked_addr, std::uint16_t picked_port, route::Candidate *out, int cap) noexcept;

// An internet invite (room + the host engine's certificate) joined through this home's PS5Upload
// at bridge_addr:bridge_port (engine /api/join/bridge). For the easy-join flow, which knows the
// invite; appended after the direct ways (it is only used when none of them answers).
bool route_bridge(const char *bridge_addr, std::uint16_t bridge_port, const char *room, const char *fp,
                  route::Candidate &out) noexcept;

// This home's PS5Upload, for the bridge: an engine discovery found on this console's own network
// (not one over Tailscale, and not the helper's forward), one that says 13.0.0 or later first.
// An older one is still given (it answers /api/join/bridge with 404, which the join says plainly).
// False: none on this network.
bool own_engine(const net::Answer *found, int n_found, char *addr, std::size_t cap, std::uint16_t *port) noexcept;

// The ways for a join the app was handed (an invite from the phone or a host, or Play again with a
// remembered host; docs/ROUTES.md §6), appended to out[n..cap): the host's own address when there
// is one (`addr` empty: none; `tailnet`: through the helper's forward; `path`: its guest socket
// under a prefix), then the bridge through this home's PS5Upload (own_addr) when the join has an
// internet invite's room and certificate. The bridge comes last: it is used only when nothing
// direct answers. Returns the new count.
int handed_ways(const char *addr, std::uint16_t port, bool tailnet, const char *path, const char *room,
                const char *fp, const char *own_addr, std::uint16_t own_port, route::Candidate *out, int n,
                int cap) noexcept;

// An invite handed over (the phone, a host's push): its direct route (the waiting room kept the one
// that answered), then the bridge for an internet invite when own_addr is set. 0: it can't be
// played from here (an internet one: this home needs a PS5Upload).
int invite_ways(const invite::Invite &inv, const char *own_addr, std::uint16_t own_port, route::Candidate *out,
                int cap) noexcept;

// Play again with a remembered host: its address (direct false: it doesn't answer, e.g. Tailscale
// is off), then the bridge with its last internet invite when own_addr is set.
int again_ways(const Friend &f, bool direct, const char *own_addr, std::uint16_t own_port, route::Candidate *out,
               int cap) noexcept;
} // namespace play
