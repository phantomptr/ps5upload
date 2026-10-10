/* The AVA1 server: one accept thread, one reader thread per connection, RPC worker
 * threads (SPEC.md §5–§9). */
#ifndef AVA1_SERVER_H
#define AVA1_SERVER_H

#include <stddef.h>
#include <stdint.h>

#include "ava1_gen.h"
#include "ava1_keys.h"

/* The data layer's view of the server (SPEC.md §12). Hooks run on a connection's reader
 * thread and must not block. */
typedef struct {
    /* A data-plane frame on the control connection of a paired session. Body borrowed. */
    int (*on_control)(const uint8_t sid[16], const uint8_t peer[32], uint8_t type, uint8_t flags,
                      const uint8_t *body, size_t len);
    /* Before reading a lane data frame's body: reserve `len` bytes (credit). Nonzero
     * refuses the frame and closes the lane. */
    int (*admit)(const uint8_t sid[16], uint16_t lane, size_t len);
    /* A lane data frame. Takes ownership of `body` (free with free()). body == NULL means
     * the admitted frame never arrived: release its reservation. Nonzero closes the lane. */
    int (*on_lane)(const uint8_t sid[16], uint16_t lane, uint8_t type, uint32_t seq, uint8_t *body,
                   size_t len);
    /* A lane joined (up = 1) or ended (up = 0). */
    void (*on_lane_change)(const uint8_t sid[16], uint16_t lane, int up);
    /* The control connection ended: park the session's jobs. */
    void (*on_session_end)(const uint8_t sid[16]);
} ava1_data_hooks_t;

typedef struct ava1_server_cfg {
    uint16_t port;              /* 0 = any free port (tests) */
    int bind_loopback;          /* 1 = 127.0.0.1 only (tests) */
    ava1_identity_t identity;
    char name[64];
    char peers_path[256];
    uint32_t ping_every_ms;
    uint32_t dead_after_ms;
    uint32_t handshake_ms;
    /* Bytes/s one frame must at least move at, after a dead_after grace; 0 = 8192. */
    uint32_t min_frame_rate;
    /* Pairing opens by itself for this long after start, but only while the peers
     * file is empty (SPEC.md §5 item 6). 0 = never by itself. */
    uint32_t pairing_window_s;
    /* Connections one source address may hold; 0 = 12. */
    uint32_t max_conns_per_ip;
    /* Sessions welcomed during a pairing window but not yet confirmed; 0 = 2. */
    uint32_t max_unpaired;
    /* Such a session ends when the window closes or after this long; 0 = 60 000 ms. */
    uint32_t pair_confirm_ms;
    /* An identical pairing request (same address, same key) is shown at most once per this
     * long; every other session shows its own code. 0 = 10 000 ms. */
    uint32_t notify_every_ms;
    /* Wrong guesses at the code one source address may make per pairing window; 0 = 5. */
    uint32_t max_pair_fails_per_ip;
    /* ... and everyone together, after which the window closes; 0 = 20. */
    uint32_t max_pair_fails_total;
    /* Pairing notifications, all addresses together: a burst of this many (0 = 3) ... */
    uint32_t notice_burst;
    /* ... then one per this long (0 = 1000 ms). When it is spent the newest session's code waits
     * and older waiting ones are dropped. */
    uint32_t notice_refill_ms;
    /* New pairing sessions one address may start per 10 s; 0 = 6. */
    uint32_t max_welcomes_per_ip;
    /* The trust slot carried a launch token (SPEC.md §5.2): a known client whose key is
     * launch_key gets ava1_launch_proof(launch_token, h) in its Welcome. No other does. */
    int has_launch;
    uint8_t launch_key[32];
    uint8_t launch_token[16];
    /* An unknown device started pairing: show its name and the code. */
    void (*on_pair_request)(const char *peer_name, uint32_t code);
    /* What an operator should know (an unreadable peers file, a pairing not stored). */
    void (*log)(const char *msg);
    /* Runs on a worker thread. Returns an AVA1 status; writes the body to out. */
    int (*rpc)(uint16_t method, const uint8_t *body, uint32_t body_len, uint8_t *out, size_t cap,
               size_t *out_len);
    /* NULL: CAP_DATA_PLANE is not advertised; a lane data frame closes the lane, a
     * control data frame is ignored. */
    const ava1_data_hooks_t *data;
    uint64_t caps; /* advertised in ServerInfo */
} ava1_server_cfg_t;

/* 0, or a negative errno. Waits up to 5 s for an earlier server's connections to end. */
int ava1_server_start(const ava1_server_cfg_t *cfg);
uint16_t ava1_server_port(void);
void ava1_server_open_pairing(uint32_t seconds);
int ava1_server_pairing_open(void);
uint32_t ava1_server_pair_guesses(void);
int ava1_server_conns(void);
/* RPC workers running right now, all sessions (they are detached: this is the only count of them). */
int ava1_server_rpc_inflight(void);
/* Stops accepting; open connections notice within one ping interval. */
void ava1_server_stop(void);
/* Identity of the paired caller while an RPC callback runs on its worker thread. */
const uint8_t *ava1_server_rpc_peer(void);

/* The waiting send, for job threads, workers and lane writers: returns when the frame
 * is written. AVA1_E_CLOSED when the session (or lane) is not live. */
int ava1_server_send(const uint8_t sid[16], uint16_t lane, uint8_t type, uint8_t flags, uint32_t channel,
                     const uint8_t *body, size_t len);
/* Like ava1_server_send, but seals `frame` in place (header room ‖ body ‖ tag room)
 * and writes it once: the send for large frames, which never copy the body. */
int ava1_server_send_frame(const uint8_t sid[16], uint16_t lane, uint8_t type, uint32_t channel,
                           uint8_t *frame, size_t body_len);
/* The non-blocking send, for hooks (which run on reader threads): copies `body` onto
 * the connection's bounded writer queue and returns at once. A full queue closes that
 * connection and returns AVA1_E_BUSY. A hook never calls ava1_server_send. */
int ava1_server_post(const uint8_t sid[16], uint16_t lane, uint8_t type, uint8_t flags, uint32_t channel,
                     const uint8_t *body, size_t len);
/* Free entries left in the session's control post queue (0..AVA1_Q_ENTRIES), or -1 when
 * the session is not live. */
int ava1_server_post_room(const uint8_t sid[16]);
/* The ids of the session's live lanes (1..AVA1_MAX_LANES); returns how many. */
int ava1_server_lanes(const uint8_t sid[16], uint16_t out[AVA1_MAX_LANES]);

/* Console to console (SPEC.md §18). On the receiving console: a ticket for `key` to send one
 * job `job` into `root`, acting for `owner` (the device asking); the token goes to the sender.
 * 0, or -1. */
int ava1_server_c2c_allow(const uint8_t key[32], const uint8_t job[16], const uint8_t owner[32], const char *root,
                          uint8_t token[16]);
/* On the sending console: dials host:port, proves the peer is `expect`, shows `token`, joins up
 * to `lanes` lanes and runs the session like an accepted one, except that the peer may make no
 * call and send nothing but its answers about `job` (it is the receiver of that one job).
 * 0 and *sid_out, or -1 with why. */
int ava1_server_dial(const char *host, uint16_t port, const uint8_t expect[32], const uint8_t token[16],
                     const uint8_t job[16], uint16_t lanes, uint8_t sid_out[16], char *why, size_t cap);
/* The session is live (not ended, not hung up). */
int ava1_server_alive(const uint8_t sid[16]);
/* Ends a session: its connections are shut down and its jobs parked as their readers leave. */
void ava1_server_hangup(const uint8_t sid[16]);
#endif
