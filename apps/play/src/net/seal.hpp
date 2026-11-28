// PS5Upload Play - a knock's secret (a room's password or a friend pass) sealed to the room's
// key, so only the host engine can read it (the Play Rooms design's 9.3, item 6): ECDH P-256 with
// a fresh key, HKDF-SHA256, AES-128-GCM over {"secret", "gid", "challenge"}; the engine's fresh
// challenge per knock makes a sealed blob useless to replay. BearSSL.
//
// blob = base64url(0x01 || ephemeral public key (65 bytes, uncompressed) || nonce (12) ||
//                  ciphertext || tag (16))
// key  = HKDF-SHA256(ikm = the ECDH x coordinate (32), salt = the challenge's bytes,
//                    info = "ps5upload room knock v1", 16 bytes)
// The plaintext is the JSON {"secret":"…","gid":"…","challenge":"<the challenge as sent>"}; no
// associated data. docs/ROOMS.md has the same.
// SPDX-License-Identifier: GPL-3.0-or-later
#pragma once
#include <cstddef>
#include <cstdint>

namespace play::seal
{
// room_pk: the room key as the ad / the challenge gives it (base64url, 65-byte uncompressed P-256
// point); challenge: base64url (the bytes are the HKDF salt). Returns the blob's length (0 on a
// bad key, no randomness, or too small an `out`).
std::size_t seal(const char *room_pk, const char *challenge, const char *gid, const char *secret, char *out,
                 std::size_t cap) noexcept;
// For tests: the same with a given ephemeral private key (32 bytes) and nonce (12 bytes).
std::size_t seal_with(const char *room_pk, const char *challenge, const char *gid, const char *secret,
                      const std::uint8_t eph_sk[32], const std::uint8_t nonce[12], char *out, std::size_t cap) noexcept;
// For tests (and the stand-in's checks): opens a blob with the room's private key; plain gets the
// JSON. False when it doesn't open.
bool open(const char *blob, const char *challenge, const std::uint8_t room_sk[32], char *plain,
          std::size_t cap) noexcept;
// base64url (no padding) decoding; returns the length or -1.
long b64url_decode(const char *in, std::uint8_t *out, std::size_t cap) noexcept;
} // namespace play::seal
