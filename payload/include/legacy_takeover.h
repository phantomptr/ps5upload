#ifndef PS5UPLOAD2_LEGACY_TAKEOVER_H
#define PS5UPLOAD2_LEGACY_TAKEOVER_H

/*
 * MIGRATION SHIM: deleted in the release after the AVA1 cutover.
 *
 * Asks a helper from before the cutover (it speaks only the old binary protocol, on two ports of its
 * own) to exit, so this instance can take over. Nothing else in the payload speaks that protocol.
 */

#define LEGACY_TAKEOVER_HEADER_LEN 28

/* Builds the old protocol's body-less takeover request (28-byte header). */
void legacy_takeover_frame(unsigned char hdr[LEGACY_TAKEOVER_HEADER_LEN]);

/* Result of legacy_takeover(). */
#define LEGACY_TAKEOVER_NONE 0    /* nothing answered on either port: no old helper */
#define LEGACY_TAKEOVER_FREED 1   /* an old helper acknowledged (or closed) and both ports are free */
#define LEGACY_TAKEOVER_STUCK (-1) /* the ports are still occupied after the last check */

/* Sends the request to loopback `mgmt_port` (or `xfer_port` for a single-port build), waits up to
 * `ack_timeout_s` for the reply, then checks up to `attempts` times, `interval_us` apart, for both
 * ports to close. */
int legacy_takeover(int mgmt_port, int xfer_port, int ack_timeout_s, int attempts, int interval_us);

/* The same, for the two ports an old helper listened on. */
int legacy_takeover_old_ports(int ack_timeout_s, int attempts, int interval_us);

#endif
