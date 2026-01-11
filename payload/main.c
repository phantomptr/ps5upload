/* Copyright (C) 2025 PS5 Upload Contributors
 *
 * This program is free software; you can redistribute it and/or modify it
 * under the terms of the GNU General Public License as published by the
 * Free Software Foundation; either version 3, or (at your option) any
 * later version.
 */

#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>
#include <sys/socket.h>
#include <netinet/in.h>
#include <arpa/inet.h>
#include <errno.h>
#include <sys/stat.h>
#include <signal.h>
#include <pthread.h>
#include <poll.h>
#include <fcntl.h>
#include <limits.h>

#include <ps5/kernel.h>

#include "config.h"
#include "storage.h"
#include "protocol.h"
#include "extract.h"
#include "notify.h"
#include "transfer.h"

static int create_server_socket(int port) {
    int sock = socket(AF_INET, SOCK_STREAM, 0);
    if(sock < 0) {
        perror("socket");
        return -1;
    }

    // Allow reuse of address
    int opt = 1;
    setsockopt(sock, SOL_SOCKET, SO_REUSEADDR, &opt, sizeof(opt));

    // Increase buffer sizes for performance
    int buf_size = 4 * 1024 * 1024; // 4MB
    setsockopt(sock, SOL_SOCKET, SO_RCVBUF, &buf_size, sizeof(buf_size));
    setsockopt(sock, SOL_SOCKET, SO_SNDBUF, &buf_size, sizeof(buf_size));
    
    // Prevent SIGPIPE on write to closed socket (BSD/PS5 specific)
    int no_sigpipe = 1;
    setsockopt(sock, SOL_SOCKET, SO_NOSIGPIPE, &no_sigpipe, sizeof(no_sigpipe));

    struct sockaddr_in addr = {0};
    addr.sin_family = AF_INET;
    addr.sin_port = htons(port);
    addr.sin_addr.s_addr = INADDR_ANY;

    if(bind(sock, (struct sockaddr*)&addr, sizeof(addr)) < 0) {
        perror("bind");
        close(sock);
        return -1;
    }

    if(listen(sock, MAX_CONNECTIONS) < 0) {
        perror("listen");
        close(sock);
        return -1;
    }

    return sock;
}

static int is_localhost(const struct sockaddr_in *addr) {
    return addr->sin_addr.s_addr == htonl(INADDR_LOOPBACK);
}

static int request_shutdown(void) {
    int sock = socket(AF_INET, SOCK_STREAM, 0);
    if(sock < 0) {
        return -1;
    }

    struct sockaddr_in addr = {0};
    addr.sin_family = AF_INET;
    addr.sin_port = htons(SERVER_PORT);
    addr.sin_addr.s_addr = htonl(INADDR_LOOPBACK);

    if(connect(sock, (struct sockaddr*)&addr, sizeof(addr)) < 0) {
        close(sock);
        return -1;
    }

    const char *cmd = "SHUTDOWN\n";
    send(sock, cmd, strlen(cmd), 0);

    char buffer[64] = {0};
    int bytes = recv(sock, buffer, sizeof(buffer) - 1, 0);
    close(sock);

    if(bytes <= 0) {
        return -1;
    }

    buffer[bytes] = '\0';
    return (strncmp(buffer, "OK", 2) == 0) ? 0 : -1;
}

typedef enum {
    CONN_CMD = 0,
    CONN_UPLOAD = 1,
} ConnMode;

struct ClientConnection {
    int sock;
    struct sockaddr_in addr;
    ConnMode mode;
    char cmd_buffer[CMD_BUFFER_SIZE];
    size_t cmd_len;
    UploadSession *upload;
    int upload_active;
};

static void close_connection(struct ClientConnection *conn) {
    if (!conn) {
        return;
    }
    if (conn->upload_active) {
        upload_session_destroy(conn->upload);
        conn->upload = NULL;
        conn->upload_active = 0;
    }
    close(conn->sock);
    conn->sock = -1;
}

static int set_nonblocking(int sock) {
    int flags = fcntl(sock, F_GETFL, 0);
    if (flags < 0) {
        return -1;
    }
    if (fcntl(sock, F_SETFL, flags | O_NONBLOCK) < 0) {
        return -1;
    }
    return 0;
}

static void set_socket_buffers(int sock) {
    int buf_size = 4 * 1024 * 1024; // 4MB
    setsockopt(sock, SOL_SOCKET, SO_RCVBUF, &buf_size, sizeof(buf_size));
    setsockopt(sock, SOL_SOCKET, SO_SNDBUF, &buf_size, sizeof(buf_size));
}

struct LegacyUploadArgs {
    int sock;
    char args[CMD_BUFFER_SIZE];
};

static void *legacy_upload_thread(void *arg) {
    struct LegacyUploadArgs *args = (struct LegacyUploadArgs *)arg;
    if (!args) {
        return NULL;
    }
    handle_upload(args->sock, args->args);
    close(args->sock);
    free(args);
    return NULL;
}

static void process_command(struct ClientConnection *conn) {
    conn->cmd_buffer[conn->cmd_len] = '\0';

#if DEBUG_LOG
    printf("Received command: %s\n", conn->cmd_buffer);
#endif

    if (strncmp(conn->cmd_buffer, "SHUTDOWN", 8) == 0) {
        if (!is_localhost(&conn->addr)) {
            const char *error = "ERROR: Unauthorized\n";
            send(conn->sock, error, strlen(error), 0);
            close_connection(conn);
        } else {
            const char *ok = "OK\n";
            send(conn->sock, ok, strlen(ok), 0);
            close_connection(conn);
            notify_info("PS5 Upload Server", "Shutting down...");
            exit(EXIT_SUCCESS);
        }
        return;
    }

    if (strncmp(conn->cmd_buffer, "LIST_STORAGE", 12) == 0) {
        handle_list_storage(conn->sock);
        close_connection(conn);
        return;
    }
    if (strncmp(conn->cmd_buffer, "LIST_DIR ", 9) == 0) {
        handle_list_dir(conn->sock, conn->cmd_buffer + 9);
        close_connection(conn);
        return;
    }
    if (strncmp(conn->cmd_buffer, "TEST_WRITE ", 11) == 0) {
        handle_test_write(conn->sock, conn->cmd_buffer + 11);
        close_connection(conn);
        return;
    }
    if (strncmp(conn->cmd_buffer, "CREATE_PATH ", 12) == 0) {
        handle_create_path(conn->sock, conn->cmd_buffer + 12);
        close_connection(conn);
        return;
    }
    if (strncmp(conn->cmd_buffer, "CHECK_DIR ", 10) == 0) {
        handle_check_dir(conn->sock, conn->cmd_buffer + 10);
        close_connection(conn);
        return;
    }
    if (strncmp(conn->cmd_buffer, "UPLOAD_V2 ", 10) == 0) {
        char dest_path[PATH_MAX];
        char mode[16] = {0};
        int parsed = sscanf(conn->cmd_buffer + 10, "%s %15s", dest_path, mode);
        if (parsed < 1) {
            const char *error = "ERROR: Invalid UPLOAD_V2 format\n";
            send(conn->sock, error, strlen(error), 0);
            close_connection(conn);
            return;
        }
        int use_temp = 1;
        if (parsed >= 2 && strcasecmp(mode, "DIRECT") == 0) {
            use_temp = 0;
        }
        conn->upload = upload_session_create(dest_path, use_temp);
        if (!conn->upload) {
            printf("[FTX] Upload init failed for %s\n", dest_path);
            const char *error = "ERROR: Upload init failed\n";
            send(conn->sock, error, strlen(error), 0);
            close_connection(conn);
            return;
        }
        conn->upload_active = 1;
        conn->mode = CONN_UPLOAD;
        conn->cmd_len = 0;
        const char *ready = "READY\n";
        send(conn->sock, ready, strlen(ready), 0);
        return;
    }
    if (strncmp(conn->cmd_buffer, "UPLOAD ", 7) == 0) {
        pthread_t tid;
        struct LegacyUploadArgs *args = malloc(sizeof(*args));
        if (!args) {
            const char *error = "ERROR: Legacy upload failed\n";
            send(conn->sock, error, strlen(error), 0);
            close_connection(conn);
            return;
        }
        args->sock = conn->sock;
        strncpy(args->args, conn->cmd_buffer + 7, sizeof(args->args) - 1);
        args->args[sizeof(args->args) - 1] = '\0';

        if (pthread_create(&tid, NULL, legacy_upload_thread, args) == 0) {
            pthread_detach(tid);
            conn->sock = -1;
            return;
        }

        free(args);
        const char *error = "ERROR: Legacy upload failed\n";
        send(conn->sock, error, strlen(error), 0);
        close_connection(conn);
        return;
    }

    const char *error = "ERROR: Unknown command\n";
    send(conn->sock, error, strlen(error), 0);
    close_connection(conn);
}

int main(void) {
    printf("╔════════════════════════════════════════╗\n");
    printf("║     PS5 Upload Server v1.0-alpha      ║\n");
    printf("║                                        ║\n");
    printf("║         Author: PhantomPtr            ║\n");
    printf("║   Fast game transfer over LAN         ║\n");
    printf("║         Port: %d                      ║\n", SERVER_PORT);
    printf("╚════════════════════════════════════════╝\n");
    printf("\n");

    // Create logging directory
    printf("[INIT] Creating log directories...\n");
    
    // Use direct mkdir instead of system() for speed
    // Set root vnode once for the lifetime of the server to ensure full FS access.
    pid_t pid = getpid();
    kernel_set_proc_rootdir(pid, kernel_get_root_vnode());
    
    mkdir("/data/ps5upload", 0777);
    mkdir("/data/ps5upload/logs", 0777);
    mkdir("/data/ps5upload/requests", 0777);

    printf("[INIT] Log directory: /data/ps5upload/logs/\n");
    printf("[INIT] Request directory: /data/ps5upload/requests/\n");

    int server_sock = create_server_socket(SERVER_PORT);
    if(server_sock < 0) {
        if(errno == EADDRINUSE) {
            printf("Port %d in use, attempting to stop existing server...\n", SERVER_PORT);
            if(request_shutdown() == 0) {
                usleep(200000);
                server_sock = create_server_socket(SERVER_PORT);
            }
        }
        if(server_sock < 0) {
            fprintf(stderr, "Failed to create server socket\n");
            return EXIT_FAILURE;
        }
    }

    printf("Server listening on port %d\n", SERVER_PORT);
    notify_info("PS5 Upload Server", "Ready on port " SERVER_PORT_STR);

    if (set_nonblocking(server_sock) != 0) {
        perror("fcntl");
        close(server_sock);
        return EXIT_FAILURE;
    }

    struct ClientConnection *connections = NULL;
    size_t conn_count = 0;
    size_t conn_cap = 0;

    while (1) {
        size_t poll_count = 1 + conn_count;
        struct pollfd *pfds = calloc(poll_count, sizeof(*pfds));
        if (!pfds) {
            usleep(1000);
            continue;
        }

        pfds[0].fd = server_sock;
        pfds[0].events = POLLIN;
        for (size_t i = 0; i < conn_count; i++) {
            pfds[i + 1].fd = connections[i].sock;
            pfds[i + 1].events = POLLIN;
        }

        int ready = poll(pfds, poll_count, 100);
        if (ready < 0) {
            free(pfds);
            continue;
        }

        if (pfds[0].revents & POLLIN) {
            while (1) {
                struct sockaddr_in client_addr;
                socklen_t client_len = sizeof(client_addr);
                int client = accept(server_sock, (struct sockaddr*)&client_addr, &client_len);
                if (client < 0) {
                    if (errno == EWOULDBLOCK || errno == EAGAIN) {
                        break;
                    }
                    perror("accept");
                    break;
                }
                if (set_nonblocking(client) != 0) {
                    close(client);
                    continue;
                }
                set_socket_buffers(client);

                if (conn_count == conn_cap) {
                    size_t new_cap = conn_cap == 0 ? 16 : conn_cap * 2;
                    struct ClientConnection *next = realloc(connections, new_cap * sizeof(*connections));
                    if (!next) {
                        close(client);
                        continue;
                    }
                    connections = next;
                    conn_cap = new_cap;
                }

                struct ClientConnection *conn = &connections[conn_count++];
                memset(conn, 0, sizeof(*conn));
                conn->sock = client;
                conn->addr = client_addr;
                conn->mode = CONN_CMD;
                conn->cmd_len = 0;
                conn->upload_active = 0;
                conn->upload = NULL;

                char client_ip[INET_ADDRSTRLEN];
                inet_ntop(AF_INET, &client_addr.sin_addr, client_ip, sizeof(client_ip));
                printf("Client connected: %s\n", client_ip);
            }
        }

        for (size_t i = 0; i < conn_count; i++) {
            if (!(pfds[i + 1].revents & POLLIN)) {
                continue;
            }
            struct ClientConnection *conn = &connections[i];
            if (conn->sock < 0) {
                continue;
            }

            if (conn->mode == CONN_CMD) {
                char buffer[1024];
                ssize_t n = recv(conn->sock, buffer, sizeof(buffer), 0);
                if (n < 0) {
                    if (errno == EWOULDBLOCK || errno == EAGAIN) {
                        continue;
                    }
                    close_connection(conn);
                    continue;
                }
                if (n == 0) {
                    close_connection(conn);
                    continue;
                }

                if (conn->cmd_len + (size_t)n >= sizeof(conn->cmd_buffer)) {
                    close_connection(conn);
                    continue;
                }
                memcpy(conn->cmd_buffer + conn->cmd_len, buffer, (size_t)n);
                conn->cmd_len += (size_t)n;

                if (memchr(conn->cmd_buffer, '\n', conn->cmd_len)) {
                    process_command(conn);
                }
            } else {
                uint8_t buffer[64 * 1024];
                ssize_t n = recv(conn->sock, buffer, sizeof(buffer), 0);
                if (n < 0) {
                    if (errno == EWOULDBLOCK || errno == EAGAIN) {
                        continue;
                    }
                    const char *error = "ERROR: Upload failed\n";
                    send(conn->sock, error, strlen(error), 0);
                    close_connection(conn);
                    continue;
                }
                if (n == 0) {
                    const char *error = "ERROR: Upload failed\n";
                    send(conn->sock, error, strlen(error), 0);
                    close_connection(conn);
                    continue;
                }

                int done = 0;
                int err = 0;
                upload_session_feed(conn->upload, buffer, (size_t)n, &done, &err);
                if (err) {
                    printf("[FTX] upload feed error\n");
                    const char *error = "ERROR: Upload failed\n";
                    send(conn->sock, error, strlen(error), 0);
                    close_connection(conn);
                    continue;
                }
                if (done) {
                    printf("[FTX] upload done\n");
                    int files = 0;
                    long long bytes = 0;
                    int finalize_ok = (upload_session_finalize(conn->upload) == 0);
                    upload_session_stats(conn->upload, &files, &bytes);
                    upload_session_destroy(conn->upload);
                    conn->upload = NULL;
                    conn->upload_active = 0;

                    if (!finalize_ok) {
                        const char *error = "ERROR: Upload finalize failed\n";
                        send(conn->sock, error, strlen(error), 0);
                        notify_error("PS5 Upload", "Upload failed");
                        close_connection(conn);
                        continue;
                    }

                    char response[256];
                    snprintf(response, sizeof(response), "SUCCESS %d %lld\n", files, bytes);
                    send(conn->sock, response, strlen(response), 0);

                    char msg[128];
                    snprintf(msg, sizeof(msg), "Transfer complete: %d files", files);
                    notify_success("PS5 Upload", msg);

                    close_connection(conn);
                }
            }
        }

        size_t write_idx = 0;
        for (size_t i = 0; i < conn_count; i++) {
            if (connections[i].sock >= 0) {
                if (write_idx != i) {
                    connections[write_idx] = connections[i];
                }
                write_idx++;
            }
        }
        conn_count = write_idx;
        free(pfds);
    }

    free(connections);

    close(server_sock);
    return EXIT_SUCCESS;
}
