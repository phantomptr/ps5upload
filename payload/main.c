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
#include <netinet/tcp.h>
#include <arpa/inet.h>
#include <errno.h>
#include <sys/stat.h>
#include <signal.h>
#include <pthread.h>
#include <poll.h>
#include <fcntl.h>
#include <limits.h>
#include <ifaddrs.h>

#include <ps5/kernel.h>

#include "config.h"
#include "storage.h"
#include "protocol.h"
#include "extract.h"
#include "notify.h"
#include "transfer.h"
#include "unrar_handler.h"
#include "extract_queue.h"

static int create_server_socket(int port) {
    int sock = socket(AF_INET, SOCK_STREAM, 0);
    if(sock < 0) {
        perror("socket");
        return -1;
    }

    // Allow reuse of address
    int opt = 1;
    setsockopt(sock, SOL_SOCKET, SO_REUSEADDR, &opt, sizeof(opt));

    // Increased to 2MB to improve throughput
    int buf_size = 2 * 1024 * 1024; // 2MB
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

static void get_local_ip(char *out, size_t out_len) {
    if (!out || out_len == 0) {
        return;
    }
    out[0] = '\0';

    struct ifaddrs *ifaddr = NULL;
    if (getifaddrs(&ifaddr) != 0) {
        return;
    }

    for (struct ifaddrs *ifa = ifaddr; ifa; ifa = ifa->ifa_next) {
        if (!ifa->ifa_addr || ifa->ifa_addr->sa_family != AF_INET) {
            continue;
        }
        struct sockaddr_in *sa = (struct sockaddr_in *)ifa->ifa_addr;
        if (sa->sin_addr.s_addr == htonl(INADDR_LOOPBACK)) {
            continue;
        }
        inet_ntop(AF_INET, &sa->sin_addr, out, out_len);
        break;
    }
    freeifaddrs(ifaddr);
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

    struct timeval tv;
    tv.tv_sec = 2; // 2s timeout
    tv.tv_usec = 0;
    setsockopt(sock, SOL_SOCKET, SO_RCVTIMEO, (const char*)&tv, sizeof tv);

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
    // Increased to 2MB to improve throughput
    int buf_size = 2 * 1024 * 1024; // 2MB
    setsockopt(sock, SOL_SOCKET, SO_RCVBUF, &buf_size, sizeof(buf_size));
    setsockopt(sock, SOL_SOCKET, SO_SNDBUF, &buf_size, sizeof(buf_size));

    // Enable TCP_NODELAY for lower latency
    int nodelay = 1;
    setsockopt(sock, IPPROTO_TCP, TCP_NODELAY, &nodelay, sizeof(nodelay));
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
            transfer_cleanup();
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
    if (strncmp(conn->cmd_buffer, "DELETE ", 7) == 0) {
        handle_delete_path(conn->sock, conn->cmd_buffer + 7);
        close_connection(conn);
        return;
    }
    if (strncmp(conn->cmd_buffer, "PROBE_RAR ", 10) == 0) {
        handle_probe_rar(conn->sock, conn->cmd_buffer + 10);
        close_connection(conn);
        return;
    }
    if (strncmp(conn->cmd_buffer, "EXTRACT_ARCHIVE ", 16) == 0) {
        handle_extract_archive(conn->sock, conn->cmd_buffer + 16);
        close_connection(conn);
        return;
    }
    if (strncmp(conn->cmd_buffer, "MOVE ", 5) == 0) {
        handle_move_path(conn->sock, conn->cmd_buffer + 5);
        close_connection(conn);
        return;
    }
    if (strncmp(conn->cmd_buffer, "COPY ", 5) == 0) {
        handle_copy_path(conn->sock, conn->cmd_buffer + 5);
        close_connection(conn);
        return;
    }
    if (strncmp(conn->cmd_buffer, "CHMOD777 ", 9) == 0) {
        handle_chmod_777(conn->sock, conn->cmd_buffer + 9);
        close_connection(conn);
        return;
    }
    if (strncmp(conn->cmd_buffer, "DOWNLOAD ", 9) == 0) {
        handle_download_file(conn->sock, conn->cmd_buffer + 9);
        close_connection(conn);
        return;
    }
    if (strncmp(conn->cmd_buffer, "DOWNLOAD_DIR ", 13) == 0) {
        handle_download_dir(conn->sock, conn->cmd_buffer + 13);
        close_connection(conn);
        return;
    }
    if (strncmp(conn->cmd_buffer, "HASH_FILE ", 10) == 0) {
        handle_hash_file(conn->sock, conn->cmd_buffer + 10);
        close_connection(conn);
        return;
    }
    if (strncmp(conn->cmd_buffer, "VERSION", 7) == 0) {
        handle_version(conn->sock);
        close_connection(conn);
        return;
    }
    if (strncmp(conn->cmd_buffer, "GET_SPACE ", 10) == 0) {
        handle_get_space(conn->sock, conn->cmd_buffer + 10);
        close_connection(conn);
        return;
    }
    if (strncmp(conn->cmd_buffer, "PAYLOAD_STATUS", 14) == 0) {
        handle_payload_status(conn->sock);
        close_connection(conn);
        return;
    }
    if (strncmp(conn->cmd_buffer, "QUEUE_EXTRACT ", 14) == 0) {
        handle_queue_extract(conn->sock, conn->cmd_buffer + 14);
        close_connection(conn);
        return;
    }
    if (strncmp(conn->cmd_buffer, "QUEUE_CANCEL ", 13) == 0) {
        handle_queue_cancel(conn->sock, conn->cmd_buffer + 13);
        close_connection(conn);
        return;
    }
    if (strncmp(conn->cmd_buffer, "QUEUE_CLEAR", 11) == 0) {
        handle_queue_clear(conn->sock);
        close_connection(conn);
        return;
    }
    if (strncmp(conn->cmd_buffer, "UPLOAD_RAR_SAFE ", 16) == 0) {
        handle_upload_rar(conn->sock, conn->cmd_buffer + 16, UNRAR_MODE_SAFE);
        close_connection(conn);
        return;
    }
    if (strncmp(conn->cmd_buffer, "UPLOAD_RAR_TURBO ", 17) == 0) {
        handle_upload_rar(conn->sock, conn->cmd_buffer + 17, UNRAR_MODE_TURBO);
        close_connection(conn);
        return;
    }
    if (strncmp(conn->cmd_buffer, "UPLOAD_RAR ", 11) == 0) {
        handle_upload_rar(conn->sock, conn->cmd_buffer + 11, UNRAR_MODE_FAST);
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
    printf("║     PS5 Upload Server v%s      ║\n", PS5_UPLOAD_VERSION);
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
    
    // Clean up any leftover temporary files from previous runs
    unrar_cleanup_temp();

    // Initialize extraction queue
    extract_queue_init();

    printf("[INIT] Log directory: /data/ps5upload/logs/\n");
    printf("[INIT] Request directory: /data/ps5upload/requests/\n");
    printf("[INIT] Extraction queue: initialized\n");

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

    char ip_buf[INET_ADDRSTRLEN] = {0};
    get_local_ip(ip_buf, sizeof(ip_buf));
    if (ip_buf[0] != '\0') {
        printf("Server listening on %s:%d\n", ip_buf, SERVER_PORT);
        char notify_msg[128];
        snprintf(notify_msg, sizeof(notify_msg), "v%s Ready on %s:%d", PS5_UPLOAD_VERSION, ip_buf, SERVER_PORT);
        notify_info("PS5 Upload Server (PhantomPtr)", notify_msg);
    } else {
        printf("Server listening on port %d\n", SERVER_PORT);
        char notify_msg[128];
        snprintf(notify_msg, sizeof(notify_msg), "v%s Ready on port %s", PS5_UPLOAD_VERSION, SERVER_PORT_STR);
        notify_info("PS5 Upload Server (PhantomPtr)", notify_msg);
    }

    if (set_nonblocking(server_sock) != 0) {
        perror("fcntl");
        close(server_sock);
        return EXIT_FAILURE;
    }

    struct ClientConnection *connections = NULL;
    size_t conn_count = 0;
    size_t conn_cap = 0;

    // Reusable poll fd array to avoid repeated malloc/free
    struct pollfd *pfds = NULL;
    size_t pfds_cap = 0;

    while (1) {
        size_t current_conn_count = conn_count;
        size_t poll_count = 1 + current_conn_count;

        // Grow pfds array if needed (but reuse existing allocation)
        if (poll_count > pfds_cap) {
            size_t new_cap = pfds_cap == 0 ? 32 : pfds_cap * 2;
            if (new_cap < poll_count) new_cap = poll_count;
            struct pollfd *new_pfds = realloc(pfds, new_cap * sizeof(*pfds));
            if (!new_pfds) {
                usleep(1000);
                continue;
            }
            pfds = new_pfds;
            pfds_cap = new_cap;
        }
        memset(pfds, 0, poll_count * sizeof(*pfds));

        pfds[0].fd = server_sock;
        pfds[0].events = POLLIN;
        for (size_t i = 0; i < current_conn_count; i++) {
            pfds[i + 1].fd = connections[i].sock;
            if (connections[i].mode == CONN_UPLOAD && upload_session_backpressure(connections[i].upload)) {
                pfds[i + 1].events = 0;
            } else {
                pfds[i + 1].events = POLLIN;
            }
        }

        int ready = poll(pfds, poll_count, 100);
        if (ready < 0) {
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

        for (size_t i = 0; i < current_conn_count; i++) {
            struct ClientConnection *conn = &connections[i];
            if (conn->sock < 0) {
                continue;
            }

            if (conn->mode == CONN_UPLOAD && upload_session_backpressure(conn->upload)) {
                int done = 0;
                int err = 0;
                upload_session_feed(conn->upload, NULL, 0, &done, &err);
                if (err) {
                    const char *error = "ERROR: Upload failed\n";
                    send(conn->sock, error, strlen(error), 0);
                    close_connection(conn);
                    continue;
                }
                if (upload_session_backpressure(conn->upload)) {
                    continue;
                }
            }

            if (!(pfds[i + 1].revents & POLLIN)) {
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
                uint8_t buffer[256 * 1024];  // Increased from 64KB to 256KB for better throughput

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
                        close_connection(conn);
                        continue;
                    }

                    char response[256];
                    snprintf(response, sizeof(response), "SUCCESS %d %lld\n", files, bytes);
                    send(conn->sock, response, strlen(response), 0);

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

        // Periodically shrink connections array if it's much larger than needed
        // This prevents unbounded memory growth from connection spikes
        if (conn_cap > 64 && conn_count < conn_cap / 4) {
            size_t new_cap = conn_cap / 2;
            struct ClientConnection *new_conns = realloc(connections, new_cap * sizeof(*connections));
            if (new_conns) {
                connections = new_conns;
                conn_cap = new_cap;
            }
        }

        // Also shrink pfds array if oversized
        if (pfds_cap > 64 && poll_count < pfds_cap / 4) {
            size_t new_cap = pfds_cap / 2;
            struct pollfd *new_pfds = realloc(pfds, new_cap * sizeof(*pfds));
            if (new_pfds) {
                pfds = new_pfds;
                pfds_cap = new_cap;
            }
        }
    }

    free(pfds);
    free(connections);

    close(server_sock);
    return EXIT_SUCCESS;
}
