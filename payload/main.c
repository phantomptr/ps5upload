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

#include <ps5/kernel.h>

#include "config.h"
#include "storage.h"
#include "protocol.h"
#include "extract.h"
#include "notify.h"

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
    int buf_size = 1024 * 1024; // 1MB
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

static void handle_client(int client_sock, const struct sockaddr_in *client_addr) {
    char cmd_buffer[CMD_BUFFER_SIZE];
    int bytes = recv(client_sock, cmd_buffer, sizeof(cmd_buffer)-1, 0);

    if(bytes <= 0) {
        close(client_sock);
        return;
    }

    cmd_buffer[bytes] = '\0';

#if DEBUG_LOG
    printf("Received command: %s\n", cmd_buffer);
#endif

    // Handle different commands
    if(strncmp(cmd_buffer, "SHUTDOWN", 8) == 0) {
        if(!is_localhost(client_addr)) {
            const char *error = "ERROR: Unauthorized\n";
            send(client_sock, error, strlen(error), 0);
        } else {
            const char *ok = "OK\n";
            send(client_sock, ok, strlen(ok), 0);
            close(client_sock);
            notify_info("PS5 Upload Server", "Shutting down...");
            exit(EXIT_SUCCESS);
        }
    }
    else if(strncmp(cmd_buffer, "LIST_STORAGE", 12) == 0) {
        handle_list_storage(client_sock);
    }
    else if(strncmp(cmd_buffer, "LIST_DIR ", 9) == 0) {
        handle_list_dir(client_sock, cmd_buffer + 9);
    }
    else if(strncmp(cmd_buffer, "TEST_WRITE ", 11) == 0) {
        handle_test_write(client_sock, cmd_buffer + 11);
    }
    else if(strncmp(cmd_buffer, "CREATE_PATH ", 12) == 0) {
        handle_create_path(client_sock, cmd_buffer + 12);
    }
    else if(strncmp(cmd_buffer, "CHECK_DIR ", 10) == 0) {
        handle_check_dir(client_sock, cmd_buffer + 10);
    }
    else if(strncmp(cmd_buffer, "UPLOAD_V2 ", 10) == 0) {
        handle_upload_v2_wrapper(client_sock, cmd_buffer + 10);
    }
    else if(strncmp(cmd_buffer, "UPLOAD ", 7) == 0) {
        handle_upload(client_sock, cmd_buffer + 7);
    }
    else {
        const char *error = "ERROR: Unknown command\n";
        send(client_sock, error, strlen(error), 0);
    }

    close(client_sock);
}

struct ClientContext {
    int sock;
    struct sockaddr_in addr;
};

static void *client_thread(void *arg) {
    struct ClientContext *ctx = (struct ClientContext *)arg;
    if (!ctx) {
        return NULL;
    }
    handle_client(ctx->sock, &ctx->addr);
    free(ctx);
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

    while(1) {
        struct sockaddr_in client_addr;
        socklen_t client_len = sizeof(client_addr);

        int client = accept(server_sock, (struct sockaddr*)&client_addr, &client_len);
        if(client < 0) {
            perror("accept");
            continue;
        }

        char client_ip[INET_ADDRSTRLEN];
        inet_ntop(AF_INET, &client_addr.sin_addr, client_ip, sizeof(client_ip));
        printf("Client connected: %s\n", client_ip);

        struct ClientContext *ctx = malloc(sizeof(*ctx));
        if (!ctx) {
            close(client);
            continue;
        }
        ctx->sock = client;
        ctx->addr = client_addr;

        pthread_t tid;
        if (pthread_create(&tid, NULL, client_thread, ctx) != 0) {
            close(client);
            free(ctx);
            continue;
        }
        pthread_detach(tid);
    }

    close(server_sock);
    return EXIT_SUCCESS;
}
