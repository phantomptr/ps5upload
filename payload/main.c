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

#define NET_LOOP_COUNT 2

struct PendingConn {
    int sock;
    struct sockaddr_in addr;
    struct PendingConn *next;
};

struct NetLoop {
    pthread_t thread;
    int notify_fds[2];
    pthread_mutex_t mutex;
    struct PendingConn *pending_head;
    struct PendingConn *pending_tail;
    struct ClientConnection *connections;
    size_t conn_count;
    size_t conn_cap;
};

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
        if (sscanf(conn->cmd_buffer + 10, "%s", dest_path) < 1) {
            const char *error = "ERROR: Invalid UPLOAD_V2 format\n";
            send(conn->sock, error, strlen(error), 0);
            close_connection(conn);
            return;
        }
        conn->upload = upload_session_create(dest_path);
        if (!conn->upload) {
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

static void netloop_add_connection(struct NetLoop *loop, int sock, struct sockaddr_in *addr) {
    struct PendingConn *node = malloc(sizeof(*node));
    if (!node) {
        close(sock);
        return;
    }
    node->sock = sock;
    node->addr = *addr;
    node->next = NULL;

    pthread_mutex_lock(&loop->mutex);
    if (!loop->pending_tail) {
        loop->pending_head = node;
        loop->pending_tail = node;
    } else {
        loop->pending_tail->next = node;
        loop->pending_tail = node;
    }
    pthread_mutex_unlock(&loop->mutex);

    char ch = 'c';
    (void)write(loop->notify_fds[1], &ch, 1);
}

static void netloop_drain_pending(struct NetLoop *loop) {
    struct PendingConn *list = NULL;
    pthread_mutex_lock(&loop->mutex);
    list = loop->pending_head;
    loop->pending_head = NULL;
    loop->pending_tail = NULL;
    pthread_mutex_unlock(&loop->mutex);

    while (list) {
        struct PendingConn *node = list;
        list = list->next;

        if (loop->conn_count == loop->conn_cap) {
            size_t new_cap = loop->conn_cap == 0 ? 16 : loop->conn_cap * 2;
            struct ClientConnection *next = realloc(loop->connections, new_cap * sizeof(*loop->connections));
            if (!next) {
                close(node->sock);
                free(node);
                continue;
            }
            loop->connections = next;
            loop->conn_cap = new_cap;
        }

        struct ClientConnection *conn = &loop->connections[loop->conn_count++];
        memset(conn, 0, sizeof(*conn));
        conn->sock = node->sock;
        conn->addr = node->addr;
        conn->mode = CONN_CMD;
        conn->cmd_len = 0;
        conn->upload_active = 0;
        conn->upload = NULL;

        char client_ip[INET_ADDRSTRLEN];
        inet_ntop(AF_INET, &node->addr.sin_addr, client_ip, sizeof(client_ip));
        printf("Client connected: %s\n", client_ip);

        free(node);
    }
}

static void netloop_compact(struct NetLoop *loop) {
    size_t write_idx = 0;
    for (size_t i = 0; i < loop->conn_count; i++) {
        if (loop->connections[i].sock >= 0) {
            if (write_idx != i) {
                loop->connections[write_idx] = loop->connections[i];
            }
            write_idx++;
        }
    }
    loop->conn_count = write_idx;
}

static void *netloop_thread(void *arg) {
    struct NetLoop *loop = (struct NetLoop *)arg;
    for (;;) {
        netloop_drain_pending(loop);

        size_t poll_count = 1 + loop->conn_count;
        struct pollfd *pfds = calloc(poll_count, sizeof(*pfds));
        if (!pfds) {
            usleep(1000);
            continue;
        }

        pfds[0].fd = loop->notify_fds[0];
        pfds[0].events = POLLIN;
        for (size_t i = 0; i < loop->conn_count; i++) {
            pfds[i + 1].fd = loop->connections[i].sock;
            pfds[i + 1].events = POLLIN;
        }

        int ready = poll(pfds, poll_count, 100);
        if (ready < 0) {
            free(pfds);
            continue;
        }

        if (pfds[0].revents & POLLIN) {
            char buf[64];
            while (read(loop->notify_fds[0], buf, sizeof(buf)) > 0) {
            }
            netloop_drain_pending(loop);
        }

        for (size_t i = 0; i < loop->conn_count; i++) {
            if (!(pfds[i + 1].revents & POLLIN)) {
                continue;
            }
            struct ClientConnection *conn = &loop->connections[i];
            if (conn->sock < 0) {
                continue;
            }

            if (conn->mode == CONN_CMD) {
                char buffer[1024];
                ssize_t n = recv(conn->sock, buffer, sizeof(buffer), 0);
                if (n <= 0) {
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
                if (n <= 0) {
                    const char *error = "ERROR: Upload failed\n";
                    send(conn->sock, error, strlen(error), 0);
                    close_connection(conn);
                    continue;
                }

                int done = 0;
                int err = 0;
                upload_session_feed(conn->upload, buffer, (size_t)n, &done, &err);
                if (err) {
                    const char *error = "ERROR: Upload failed\n";
                    send(conn->sock, error, strlen(error), 0);
                    close_connection(conn);
                    continue;
                }
                if (done) {
                    int files = 0;
                    long long bytes = 0;
                    upload_session_stats(conn->upload, &files, &bytes);
                    upload_session_destroy(conn->upload);
                    conn->upload = NULL;
                    conn->upload_active = 0;

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

        netloop_compact(loop);
        free(pfds);
    }
    return NULL;
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

    struct NetLoop loops[NET_LOOP_COUNT];
    for (int i = 0; i < NET_LOOP_COUNT; i++) {
        memset(&loops[i], 0, sizeof(loops[i]));
        if (pipe(loops[i].notify_fds) != 0) {
            fprintf(stderr, "Failed to create notify pipe\n");
            return EXIT_FAILURE;
        }
        set_nonblocking(loops[i].notify_fds[0]);
        pthread_mutex_init(&loops[i].mutex, NULL);
        pthread_create(&loops[i].thread, NULL, netloop_thread, &loops[i]);
    }

    size_t rr = 0;
    while (1) {
        struct sockaddr_in client_addr;
        socklen_t client_len = sizeof(client_addr);
        int client = accept(server_sock, (struct sockaddr*)&client_addr, &client_len);
        if (client < 0) {
            if (errno == EINTR) {
                continue;
            }
            perror("accept");
            continue;
        }
        if (set_nonblocking(client) != 0) {
            close(client);
            continue;
        }
        set_socket_buffers(client);

        struct NetLoop *loop = &loops[rr % NET_LOOP_COUNT];
        rr++;
        netloop_add_connection(loop, client, &client_addr);
    }

    close(server_sock);
    return EXIT_SUCCESS;
}
