#ifndef PROTOCOL_H
#define PROTOCOL_H

void handle_test_write(int client_sock, const char *path);
void handle_create_path(int client_sock, const char *path);
void handle_check_dir(int client_sock, const char *path);
void handle_upload(int client_sock, const char *args);
void handle_upload_v2_wrapper(int client_sock, const char *args);
void handle_delete_path(int client_sock, const char *path);
void handle_move_path(int client_sock, const char *args);
void handle_copy_path(int client_sock, const char *args);
void handle_chmod_777(int client_sock, const char *path);
void handle_download_file(int client_sock, const char *path);
void handle_download_dir(int client_sock, const char *path);
void handle_hash_file(int client_sock, const char *path);
void handle_version(int client_sock);

#endif
