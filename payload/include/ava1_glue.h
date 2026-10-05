#ifndef PS5UPLOAD_AVA1_GLUE_H
#define PS5UPLOAD_AVA1_GLUE_H

/* Starts the AVA1 server on 9120. 0 on success; 0 on success, or an error when the server could not start. */
int ava1_payload_start(void);
/* The AVA1 side was deliberately not started (another process still answers its port): Hello then
 * reports "failed", and ava1_payload_stop has nothing to stop. */
void ava1_payload_refused(void);

/* "starting", "up" or "failed": the AVA1 server's state, reported in the old Hello reply. */
const char *ava1_payload_state(void);

#endif
