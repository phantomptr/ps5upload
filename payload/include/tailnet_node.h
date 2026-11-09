/* "Put this PS5 on Tailscale": the helper's tailnet node (tailnet/ in the repo root).
 *
 * When enabled, the console joins the user's tailnet with its keys under
 * /data/ps5upload/tailnet and serves exactly four ports to it, an allow-list enforced
 * in the core (tailnet/src/dp.c inbound_ok) on top of the tailnet's own ACL:
 *   TCP 9295  Remote Play session        -> 127.0.0.1:9295 (lwIP, spliced)
 *   UDP 9296  Remote Play stream         -> 127.0.0.1:9296 (relayed)
 *   UDP 9302  Remote Play discovery/wake -> 127.0.0.1:9302 (relayed)
 *   TCP 9120  this helper (AVA1)         -> 127.0.0.1:9120 (lwIP, spliced)
 * Nothing else reaches the console from the tailnet, and the console never dials out
 * over it. Rest mode: the wake watchdog's power phase (power_phase.h) pauses the node
 * (no sockets) while the console goes to rest and until it has settled after a wake, then
 * resumes it; the core also notices the clock jump on wake by itself, rebinds,
 * re-handshakes and opens a new control session; errno 163 on any socket is a rebuild
 * with backoff, never fatal. */
#ifndef PS5UPLOAD_TAILNET_NODE_H
#define PS5UPLOAD_TAILNET_NODE_H

#include <stddef.h>

#define TAILNET_NODE_DIR "/data/ps5upload/tailnet"
#define TAILNET_DEFAULT_CONTROL "https://controlplane.tailscale.com"

/* At helper start: join the tailnet if the saved settings say so. 0, or -1 (logged). */
int tailnet_node_boot(void);
/* At helper exit. */
void tailnet_node_shutdown(void);

/* The tailnet management call (AVA1 method tailnet). Body is JSON:
 *   {"op":"status"}                                   (also an empty body)
 *   {"op":"enable","control_url":"https://…","hostname":"…","authkey":"…","control_addr":"a.b.c.d"}
 *       all fields optional; control_addr is the engine's DNS answer for the control host
 *       (the console has no resolver of its own)
 *   {"op":"disable"}                                  leave the tailnet, keep the keys
 *   {"op":"logout"}                                   leave and forget the keys (sign out)
 *   {"op":"rename","hostname":"…"}                    the device name, any time (applied now
 *       when on: the node starts again under it; the address stays)
 *   {"op":"renew"}                                    renew the sign-in, same node (a login URL)
 *   {"op":"allow","allow_mode":"all|listed","allow":"100.a.b.c,100.d.e.f"}
 *       which tailnet devices may reach the PS5: every one the tailnet allows, or only these
 *   {"op":"allow_add","ip":"100.a.b.c"}               add one device to the list (the app adds
 *       itself when it talks to the PS5 at home)
 *   {"op":"blocked_ignore","ip":"100.a.b.c"}          stop showing a device the list turned
 *       away (in memory, until the helper restarts)
 * Answers the status JSON (see tailnet_node.c status_json) into out. Its "blocked_recent" lists
 * the devices the list turned away lately (the core's tn_peer_denied: at most 8, one per
 * address, nothing logged). Returns its length or -1. */
int tailnet_node_call(const char *body, size_t len, char *out, size_t cap);

/* Rest mode (power_phase.c): 1 drops every connection and keeps the node idle with no sockets
 * (tn_pause) while the console goes to rest and until it has settled after a wake; 0 rejoins
 * (tn_resume: new control session, UDP rebind, DERP, netcheck). Kept while the node is off, so a
 * node started meanwhile starts paused. Never blocks: -1 when a start or stop holds the node's
 * lock (ask again later), else 0. */
int tailnet_node_set_paused(int paused);
/* The value last set (tests, diagnostics). */
int tailnet_node_paused(void);
/* For a log line: "off", "on", "off,paused", "on,paused", or "busy" while a start or stop holds
 * the node's lock. Never blocks. */
const char *tailnet_node_brief(void);

/* Is this tailnet IPv4 (host order) allowed by the saved list? For tests. */
int tailnet_node_allows(unsigned int ip);
/* Tests: what waking from rest mode runs in the core (the wall clock jumped). */
void tailnet_node_test_rest_wake(void);
/* Tests: a TCP connection from this tailnet IPv4 (host order) to `port`, as the core notes one
 * the list turns away (nothing when the node is off or the address is let in). */
void tailnet_node_test_denied(unsigned int ip, unsigned int port);

/* Tests: use another state directory (before anything else). */
void tailnet_node_set_dir(const char *dir);

#endif
