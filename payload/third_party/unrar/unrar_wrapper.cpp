/* C wrapper implementation for unrar library */

#include "rar.hpp"
#include "dll.hpp"
#include "unrar_wrapper.h"

#include <string.h>
#include <stdlib.h>
#include <time.h>
#include <ctype.h>
#include <string>
#include <vector>

static bool sanitize_target_path(const char *input, std::string &out) {
    if (!input || !*input) {
        return false;
    }

    const char *p = input;
    if (isalpha((unsigned char)p[0]) && p[1] == ':') {
        p += 2;
    }

    while (*p == '/' || *p == '\\') {
        p++;
    }

    std::vector<std::string> segments;
    while (*p != '\0') {
        while (*p == '/' || *p == '\\') {
            p++;
        }
        const char *start = p;
        while (*p != '\0' && *p != '/' && *p != '\\') {
            p++;
        }
        if (p == start) {
            continue;
        }
        std::string segment(start, (size_t)(p - start));
        if (segment == "." || segment.empty()) {
            continue;
        }
        if (segment == "..") {
            if (!segments.empty()) {
                segments.pop_back();
            }
            continue;
        }
        segments.push_back(segment);
    }

    if (segments.empty()) {
        return false;
    }

    out.clear();
    for (size_t i = 0; i < segments.size(); i++) {
        if (i > 0) {
            out.push_back('/');
        }
        out.append(segments[i]);
    }
    return true;
}

/* Progress callback context */
struct ExtractContext {
    unrar_progress_cb callback;
    void *user_data;
    int files_done;
    int abort_flag;
    /* Fields for keep-alive updates */
    char current_filename[1024];
    unsigned long long current_file_size;
    unsigned long long total_processed;
    unsigned long long progress_total_size;
    unsigned long long total_unpacked_size;
    unsigned long long bytes_since_sleep;
    time_t last_update_time;
    unsigned int keepalive_interval_sec;
    unsigned long long sleep_every_bytes;
    unsigned int sleep_us;
    int use_dynamic_total;
};

/* Internal callback for unrar library */
static int CALLBACK unrar_callback(UINT msg, LPARAM user_data, LPARAM p1, LPARAM p2) {
    ExtractContext *ctx = (ExtractContext *)user_data;
    if (!ctx) return 0;

    switch (msg) {
        case UCM_PROCESSDATA:
            /* p1 = data pointer, p2 = data size */
            if (p2 > 0) {
                ctx->total_processed += (unsigned long long)p2;
                ctx->bytes_since_sleep += (unsigned long long)p2;
            }
            if (ctx->callback) {
                time_t now = time(NULL);
                /* Send keep-alive update every 5 seconds for large files */
                /* Or if we just processed a chunk? No, stick to time to avoid spam */
                if (ctx->keepalive_interval_sec > 0 &&
                    now - ctx->last_update_time >= (time_t)ctx->keepalive_interval_sec) {
                    ctx->last_update_time = now;
                    /* Re-send the current file status to keep the client connection alive */
                    if (ctx->callback(ctx->current_filename, ctx->current_file_size, ctx->files_done,
                                      ctx->total_processed, ctx->progress_total_size, ctx->user_data) != 0) {
                        ctx->abort_flag = 1;
                        return -1;
                    }
                }
            }
            /* Throttle CPU usage to prevent OS kill/watchdog timeout */
            /* Yield periodically based on configured thresholds */
            if (ctx->sleep_every_bytes > 0 && ctx->sleep_us > 0 &&
                ctx->bytes_since_sleep > ctx->sleep_every_bytes) {
                usleep(ctx->sleep_us);
                ctx->bytes_since_sleep = 0;
            }
            break;
        case UCM_NEEDPASSWORD:
        case UCM_NEEDPASSWORDW:
            /* Password required - we don't support encrypted archives yet */
            ctx->abort_flag = 1;
            return -1;
        case UCM_CHANGEVOLUME:
        case UCM_CHANGEVOLUMEW:
            /* Multi-volume archives - allow it */
            return 1;
    }
    return ctx->abort_flag ? -1 : 1;
}

extern "C" int unrar_extract(const char *rar_path, const char *dest_dir, int strip_root,
                              unsigned long long progress_total_size, const unrar_extract_opts *opts,
                              unrar_progress_cb progress, void *user_data,
                              int *file_count, unsigned long long *total_size) {
    if (!rar_path || !dest_dir) {
        return UNRAR_ERR_OPEN;
    }

    unrar_extract_opts local_opts;
    if (opts) {
        local_opts = *opts;
    } else {
        memset(&local_opts, 0, sizeof(local_opts));
    }

    struct RAROpenArchiveData arc_data;
    memset(&arc_data, 0, sizeof(arc_data));
    arc_data.ArcName = (char *)rar_path;
    arc_data.OpenMode = RAR_OM_EXTRACT;

    HANDLE hArc = RAROpenArchive(&arc_data);
    if (!hArc || arc_data.OpenResult != ERAR_SUCCESS) {
        return UNRAR_ERR_OPEN;
    }

    ExtractContext ctx;
    ctx.callback = progress;
    ctx.user_data = user_data;
    ctx.files_done = 0;
    ctx.abort_flag = 0;
    ctx.last_update_time = time(NULL);
    memset(ctx.current_filename, 0, sizeof(ctx.current_filename));
    ctx.current_file_size = 0;
    ctx.total_processed = 0;
    ctx.progress_total_size = progress_total_size;
    ctx.total_unpacked_size = 0;
    ctx.bytes_since_sleep = 0;
    ctx.keepalive_interval_sec = local_opts.keepalive_interval_sec;
    ctx.sleep_every_bytes = local_opts.sleep_every_bytes;
    ctx.sleep_us = local_opts.sleep_us;
    ctx.use_dynamic_total = (progress_total_size == 0);

    RARSetCallback(hArc, unrar_callback, (LPARAM)&ctx);

    struct RARHeaderDataEx header;
    int result = UNRAR_OK;

    while (1) {
        memset(&header, 0, sizeof(header));
        int read_result = RARReadHeaderEx(hArc, &header);
        if (read_result == ERAR_END_ARCHIVE) {
            break;
        }
        if (read_result != ERAR_SUCCESS) {
            result = UNRAR_ERR_READ;
            break;
        }

        /* Update context for callback */
        strncpy(ctx.current_filename, header.FileName, sizeof(ctx.current_filename) - 1);
        ctx.current_filename[sizeof(ctx.current_filename) - 1] = '\0';
        unsigned long long file_size = ((unsigned long long)header.UnpSizeHigh << 32) | header.UnpSize;
        ctx.current_file_size = file_size;
        if (ctx.use_dynamic_total) {
            ctx.progress_total_size += file_size;
        }
        ctx.last_update_time = time(NULL);

        /* Report progress before extraction */
        if (progress) {
            if (progress(header.FileName, file_size, ctx.files_done, ctx.total_processed, ctx.progress_total_size, user_data) != 0) {
                result = UNRAR_ERR_EXTRACT;
                break;
            }
        }

        /* Determine destination path */
        char *target_name = header.FileName;
        if (strip_root) {
            char *slash = strchr(target_name, '/');
            char *backslash = strchr(target_name, '\\');
            if (backslash && (!slash || backslash < slash)) {
                slash = backslash;
            }
            
            if (slash) {
                target_name = slash + 1;
            }
        }

        std::string sanitized;
        if (!sanitize_target_path(target_name, sanitized)) {
            RARProcessFile(hArc, RAR_SKIP, NULL, NULL);
            continue;
        }

        std::string full_dest = std::string(dest_dir) + "/" + sanitized;

        /* Extract the file */
        /* Note: When providing DestName (2nd arg for path), DestPath (1st arg) is ignored or handled differently
           depending on implementation. We use DestName to control full path. */
        std::vector<char> full_dest_buf(full_dest.begin(), full_dest.end());
        full_dest_buf.push_back('\0');
        int proc_result = RARProcessFile(hArc, RAR_EXTRACT, NULL, full_dest_buf.data());
        if (proc_result != ERAR_SUCCESS) {
            if (proc_result == ERAR_MISSING_PASSWORD || proc_result == ERAR_BAD_PASSWORD) {
                result = UNRAR_ERR_PASSWORD;
            } else {
                result = UNRAR_ERR_EXTRACT;
            }
            break;
        }

        if (!(header.Flags & RHDF_DIRECTORY)) {
            ctx.files_done++;
            ctx.total_unpacked_size += file_size;
        }

        if (ctx.abort_flag) {
            result = UNRAR_ERR_EXTRACT;
            break;
        }
    }

    RARCloseArchive(hArc);
    if (file_count) {
        *file_count = ctx.files_done;
    }
    if (total_size) {
        *total_size = ctx.total_unpacked_size;
    }
    return result;
}

extern "C" int unrar_scan(const char *rar_path, int *file_count, unsigned long long *total_size, 
                         char *common_root, size_t root_len) {
    if (!rar_path) {
        return UNRAR_ERR_OPEN;
    }

    struct RAROpenArchiveData arc_data;
    memset(&arc_data, 0, sizeof(arc_data));
    arc_data.ArcName = (char *)rar_path;
    arc_data.OpenMode = RAR_OM_LIST;

    HANDLE hArc = RAROpenArchive(&arc_data);
    if (!hArc || arc_data.OpenResult != ERAR_SUCCESS) {
        return UNRAR_ERR_OPEN;
    }

    int count = 0;
    unsigned long long size = 0;
    struct RARHeaderDataEx header;

    char first_root[260] = {0};
    int multiple_roots = 0;

    if (common_root && root_len > 0) {
        common_root[0] = '\0';
    }

    while (1) {
        memset(&header, 0, sizeof(header));
        int read_result = RARReadHeaderEx(hArc, &header);
        if (read_result == ERAR_END_ARCHIVE) {
            break;
        }
        if (read_result != ERAR_SUCCESS) {
            RARCloseArchive(hArc);
            return UNRAR_ERR_READ;
        }

        /* Skip directories */
        if (!(header.Flags & RHDF_DIRECTORY)) {
            count++;
            size += ((unsigned long long)header.UnpSizeHigh << 32) | header.UnpSize;

            if (common_root) {
                char current_root[260] = {0};
                char *slash = strchr(header.FileName, '/');
                char *backslash = strchr(header.FileName, '\\');
                if (backslash && (!slash || backslash < slash)) {
                    slash = backslash;
                }

                if (slash) {
                    size_t len = slash - header.FileName;
                    if (len < sizeof(current_root)) {
                        strncpy(current_root, header.FileName, len);
                        current_root[len] = '\0';
                    }
                } else {
                    /* File at root, so no common folder */
                    multiple_roots = 1;
                }

                if (!multiple_roots) {
                    if (first_root[0] == '\0') {
                        strncpy(first_root, current_root, sizeof(first_root) - 1);
                    } else if (strcmp(first_root, current_root) != 0) {
                        multiple_roots = 1;
                    }
                }
            }
        }

        /* Skip to next header without extracting */
        RARProcessFile(hArc, RAR_SKIP, NULL, NULL);
    }

    RARCloseArchive(hArc);

    if (file_count) *file_count = count;
    if (total_size) *total_size = size;
    
    if (common_root && root_len > 0 && !multiple_roots && first_root[0] != '\0') {
        snprintf(common_root, root_len, "%s", first_root);
    }

    return UNRAR_OK;
}

extern "C" const char *unrar_strerror(int err) {
    switch (err) {
        case UNRAR_OK:          return "Success";
        case UNRAR_ERR_OPEN:    return "Cannot open archive";
        case UNRAR_ERR_READ:    return "Error reading archive";
        case UNRAR_ERR_EXTRACT: return "Extraction failed";
        case UNRAR_ERR_PASSWORD: return "Password required";
        case UNRAR_ERR_MEMORY:  return "Out of memory";
        case UNRAR_ERR_BADARCHIVE: return "Invalid archive format";
        default:                return "Unknown error";
    }
}
