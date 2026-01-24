#ifndef PROTOCOL_H
#define PROTOCOL_H

#include <string.h>

static inline int is_path_safe(const char *path) {
    if (strstr(path, "..")) {
        return 0;
    }
    if (strncmp(path, "/data/", 6) != 0) {
        return 0;
    }
    return 1;
}

void handle_test_write(int client_sock, const char *path);
void handle_create_path(int client_sock, const char *path);
void handle_check_dir(int client_sock, const char *path);
void handle_upload(int client_sock, const char *args);
void handle_upload_v2_wrapper(int client_sock, const char *args);
void handle_delete_path(int client_sock, const char *path);
void handle_move_path(int client_sock, const char *args);
void handle_copy_path(int client_sock, const char *args);
void handle_extract_archive(int client_sock, const char *args);
void handle_probe_rar(int client_sock, const char *args);
void handle_chmod_777(int client_sock, const char *path);
void handle_download_file(int client_sock, const char *path);
void handle_download_dir(int client_sock, const char *path);
void handle_hash_file(int client_sock, const char *path);
void handle_get_space(int client_sock, const char *path);
void handle_version(int client_sock);

/* Extraction queue handlers */
void handle_payload_status(int client_sock);
void handle_queue_extract(int client_sock, const char *args);
void handle_queue_cancel(int client_sock, const char *args);
void handle_queue_clear(int client_sock);

#endif
