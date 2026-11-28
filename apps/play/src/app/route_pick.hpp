// PS5Upload Play - the ways to one engine, from what discovery found (docs/ROUTES.md).
// SPDX-License-Identifier: GPL-3.0-or-later
#pragma once
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
} // namespace play
