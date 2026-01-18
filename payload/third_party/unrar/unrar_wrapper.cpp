/* C wrapper implementation for unrar library */

#include "rar.hpp"
#include "dll.hpp"
#include "unrar_wrapper.h"

#include <string.h>
#include <stdlib.h>

/* Progress callback context */
struct ExtractContext {
    unrar_progress_cb callback;
    void *user_data;
    int files_done;
    int abort_flag;
    /* Fields for keep-alive updates */
    char current_filename[1024];
    unsigned long long current_file_size;
    time_t last_update_time;
};

/* Internal callback for unrar library */
static int CALLBACK unrar_callback(UINT msg, LPARAM user_data, LPARAM p1, LPARAM p2) {
    ExtractContext *ctx = (ExtractContext *)user_data;
    if (!ctx) return 0;

    switch (msg) {
        case UCM_PROCESSDATA:
            /* p1 = data pointer, p2 = data size */
            if (ctx->callback) {
                time_t now = time(NULL);
                /* Send keep-alive update every 5 seconds for large files */
                if (now - ctx->last_update_time >= 5) {
                    ctx->last_update_time = now;
                    /* Re-send the current file status to keep the client connection alive */
                    if (ctx->callback(ctx->current_filename, ctx->current_file_size, ctx->files_done, ctx->user_data) != 0) {
                        ctx->abort_flag = 1;
                        return -1;
                    }
                }
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
                              unrar_progress_cb progress, void *user_data) {
    if (!rar_path || !dest_dir) {
        return UNRAR_ERR_OPEN;
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

    RARSetCallback(hArc, unrar_callback, (LPARAM)&ctx);

    struct RARHeaderData header;
    int result = UNRAR_OK;

    while (1) {
        int read_result = RARReadHeader(hArc, &header);
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
        ctx.current_file_size = header.UnpSize;
        ctx.last_update_time = time(NULL);

        /* Report progress before extraction */
        if (progress) {
            unsigned long long file_size = header.UnpSize;
            if (progress(header.FileName, file_size, ctx.files_done, user_data) != 0) {
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

        char full_dest[1024];
        snprintf(full_dest, sizeof(full_dest), "%s/%s", dest_dir, target_name);

        /* Extract the file */
        /* Note: When providing DestName (2nd arg for path), DestPath (1st arg) is ignored or handled differently
           depending on implementation. We use DestName to control full path. */
        int proc_result = RARProcessFile(hArc, RAR_EXTRACT, NULL, full_dest);
        if (proc_result != ERAR_SUCCESS) {
            if (proc_result == ERAR_MISSING_PASSWORD || proc_result == ERAR_BAD_PASSWORD) {
                result = UNRAR_ERR_PASSWORD;
            } else {
                result = UNRAR_ERR_EXTRACT;
            }
            break;
        }

        ctx.files_done++;

        if (ctx.abort_flag) {
            result = UNRAR_ERR_EXTRACT;
            break;
        }
    }

    RARCloseArchive(hArc);
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
    struct RARHeaderData header;

    char first_root[260] = {0};
    int multiple_roots = 0;

    if (common_root && root_len > 0) {
        common_root[0] = '\0';
    }

    while (1) {
        int read_result = RARReadHeader(hArc, &header);
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
            size += header.UnpSize;

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
