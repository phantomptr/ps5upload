#ifndef TRANSFER_H
#define TRANSFER_H

#include <stdint.h>
#include <stddef.h>
#include <time.h>

typedef struct UploadSession UploadSession;

typedef struct {
    size_t pack_in_use;
    int pool_count;
    size_t queue_count;
    size_t pack_queue_count;
    int active_sessions;
    uint64_t backpressure_events;
    uint64_t backpressure_wait_ms;
    uint64_t bytes_received;
    uint64_t bytes_written;
    uint64_t recv_rate_bps;
    uint64_t write_rate_bps;
    int tune_level;
    uint64_t recommend_pack_limit;
    uint64_t recommend_pace_ms;
    uint64_t recommend_rate_limit_bps;
    time_t last_progress;
    int abort_requested;
    int workers_initialized;
    time_t abort_at;
    uint64_t abort_session_id;
    char abort_reason[128];
} TransferStats;

UploadSession *upload_session_create(const char *dest_root, int use_temp);
int upload_session_finalize(UploadSession *session);
void upload_session_destroy(UploadSession *session);
int upload_session_feed(UploadSession *session, const uint8_t *data, size_t len, int *done, int *error);
int upload_session_backpressure(UploadSession *session);
void upload_session_stats(UploadSession *session, int *files, unsigned long long *bytes);

// Legacy blocking handler for compatibility
void handle_upload_v3(int client_sock, const char *dest_root, int use_temp, int chmod_each_file, int chmod_final);

void transfer_cleanup(void);
int transfer_idle_cleanup(void);
void transfer_request_abort(void);
void transfer_request_abort_with_reason(const char *reason);
int transfer_abort_requested(void);
int transfer_is_active(void);
time_t transfer_last_progress(void);
int transfer_get_stats(TransferStats *out);

#endif
