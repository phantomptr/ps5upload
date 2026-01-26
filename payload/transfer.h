#ifndef TRANSFER_H
#define TRANSFER_H

#include <stdint.h>
#include <stddef.h>
#include <time.h>

typedef struct UploadSession UploadSession;

UploadSession *upload_session_create(const char *dest_root, int use_temp);
int upload_session_finalize(UploadSession *session);
void upload_session_destroy(UploadSession *session);
int upload_session_feed(UploadSession *session, const uint8_t *data, size_t len, int *done, int *error);
int upload_session_backpressure(UploadSession *session);
void upload_session_stats(UploadSession *session, int *files, long long *bytes);

// Legacy blocking handler for compatibility
void handle_upload_v2(int client_sock, const char *dest_root, int use_temp);

void transfer_cleanup(void);
int transfer_idle_cleanup(void);
void transfer_request_abort(void);
int transfer_abort_requested(void);
int transfer_is_active(void);
time_t transfer_last_progress(void);

#endif
