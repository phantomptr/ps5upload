#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>
#include <sys/socket.h>
#include <sys/stat.h>
#include <errno.h>
#include <limits.h>
#include <fcntl.h>

#include "protocol_defs.h"
#include "notify.h"

#define PACK_BUFFER_SIZE (32 * 1024 * 1024) // 32MB buffer for packs

// Helper to receive exact bytes
static int recv_exact(int sock, void *buf, size_t len) {
    size_t received = 0;
    while(received < len) {
        ssize_t n = recv(sock, (char*)buf + received, len - received, 0);
        if(n <= 0) return -1;
        received += n;
    }
    return 0;
}

// Helper to create directories recursively with caching
static int mkdir_recursive(const char *path, char *cache) {
    if (cache && strcmp(path, cache) == 0) {
        return 0; // Already created this directory
    }

    char tmp[PATH_MAX];
    char *p = NULL;
    size_t len;

    snprintf(tmp, sizeof(tmp), "%s", path);
    len = strlen(tmp);
    if (tmp[len - 1] == '/')
        tmp[len - 1] = 0;

    for (p = tmp + 1; *p; p++) {
        if (*p == '/') {
            *p = 0;
            // Optimization: We could check here too, but checking the full leaf path is usually enough
            if (mkdir(tmp, 0777) != 0 && errno != EEXIST) {
                return -1;
            }
            *p = '/';
        }
    }
    if (mkdir(tmp, 0777) != 0 && errno != EEXIST) {
        return -1;
    }

    if (cache) {
        strncpy(cache, path, PATH_MAX - 1);
        cache[PATH_MAX - 1] = '\0';
    }
    return 0;
}

void handle_upload_v2(int client_sock, const char *dest_root) {
    printf("[FTX] Starting V2 Upload to %s\n", dest_root);
    
    // Send READY (legacy handshake compatibility)
    // The client expects "READY\n" after sending UPLOAD command
    const char *ready = "READY\n";
    send(client_sock, ready, strlen(ready), 0);

    // Prepare Pack Buffer
    uint8_t *pack_buf = malloc(PACK_BUFFER_SIZE);
    if(!pack_buf) {
        const char *err = "ERROR: OOM\n";
        send(client_sock, err, strlen(err), 0);
        return;
    }

    struct FrameHeader header;
    int total_files = 0;
    long long total_bytes = 0;
    char last_path[PATH_MAX] = {0};
    char dir_cache[PATH_MAX] = {0}; // Cache for last created directory

    // Ensure root exists
    mkdir_recursive(dest_root, dir_cache);

    while(1) {
        // Read Frame Header
        if(recv_exact(client_sock, &header, sizeof(header)) != 0) {
            printf("[FTX] Connection lost reading header\n");
            break;
        }

        if(header.magic != MAGIC_FTX1) {
            printf("[FTX] Invalid Magic: %08x\n", header.magic);
            break;
        }

        if(header.type == FRAME_FINISH) {
            printf("[FTX] Received FINISH\n");
            break;
        }
        else if(header.type == FRAME_PACK) {
            if(header.body_len > PACK_BUFFER_SIZE) {
                printf("[FTX] Pack too large: %llu\n", (unsigned long long)header.body_len);
                break; // TODO: Handle large packs by streaming? For now reject.
            }

            // Read Pack Body
            if(recv_exact(client_sock, pack_buf, header.body_len) != 0) {
                printf("[FTX] Failed to read pack body\n");
                break;
            }

            // Parse Records
            size_t offset = 0;
            uint32_t record_count = 0;
            
            // First 4 bytes: Record Count
            if(header.body_len < 4) break;
            memcpy(&record_count, pack_buf, 4);
            offset += 4;

            for(uint32_t i=0; i<record_count; i++) {
                if(offset + 2 > header.body_len) break;
                
                uint16_t path_len;
                memcpy(&path_len, pack_buf + offset, 2);
                offset += 2;

                if(offset + path_len + 8 > header.body_len) break;

                char rel_path[PATH_MAX];
                memcpy(rel_path, pack_buf + offset, path_len);
                rel_path[path_len] = '\0';
                offset += path_len;

                uint64_t data_len;
                memcpy(&data_len, pack_buf + offset, 8);
                offset += 8;

                if(offset + data_len > header.body_len) break;

                // Write File
                char full_path[PATH_MAX];
                snprintf(full_path, sizeof(full_path), "%s/%s", dest_root, rel_path);

                // Create parent dirs if needed
                char *last_slash = strrchr(full_path, '/');
                if(last_slash) {
                    *last_slash = '\0';
                    // Pass the directory path to the cached recursive mkdir
                    mkdir_recursive(full_path, dir_cache);
                    *last_slash = '/';
                }

                // Determine mode: Append if same file, Write (truncate) if new
                const char *mode = "wb";
                if(strncmp(rel_path, last_path, PATH_MAX) == 0) {
                    mode = "ab";
                } else {
                    // New file, update stats
                    total_files++;
                    strncpy(last_path, rel_path, PATH_MAX);
                }

                FILE *fp = fopen(full_path, mode);
                if(fp) {
                    fwrite(pack_buf + offset, 1, data_len, fp);
                    fclose(fp);
                    total_bytes += data_len;
                } else {
                    printf("[FTX] Failed to open %s: %s\n", full_path, strerror(errno));
                }

                offset += data_len;
            }
        }
    }

    free(pack_buf);

    // Send Success Response (Legacy Format for now to keep client logic simple)
    // SUCCESS <files> <bytes>\n
    char response[256];
    snprintf(response, sizeof(response), "SUCCESS %d %lld\n", total_files, total_bytes);
    send(client_sock, response, strlen(response), 0);
    
    char msg[128];
    snprintf(msg, sizeof(msg), "Transfer complete: %d files", total_files);
    notify_success("PS5 Upload", msg);
}
