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
 * over it. Rest mode: the core notices the clock jump on wake, rebinds, re-handshakes
 * and opens a new control session by itself; errno 163 on any socket is a rebuild
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
 * Answers the status JSON (see tailnet_node.c status_json) into out. Returns its length
 * or -1. */
int tailnet_node_call(const char *body, size_t len, char *out, size_t cap);

/* Tests: use another state directory (before anything else). */
void tailnet_node_set_dir(const char *dir);

#endif
