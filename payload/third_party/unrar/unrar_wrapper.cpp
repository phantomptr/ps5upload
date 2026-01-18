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
#include <sys/stat.h>
#if defined(_WIN32)
#include <windows.h>
#else
#include <unistd.h>
#endif
#include <limits.h>
#include <errno.h>

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

static void sleep_us(unsigned int usec) {
#if defined(_WIN32)
    if (usec == 0) {
        return;
    }
    Sleep((usec + 999) / 1000);
#else
    usleep(usec);
#endif
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
    RAR_UNUSED(p1);

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
                sleep_us(ctx->sleep_us);
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

struct RarProbeEntry {
    std::string original;
    std::string normalized;
    std::string lower;
};

static std::string normalize_path(const std::string &input) {
    std::string out = input;
    for (size_t i = 0; i < out.size(); i++) {
        if (out[i] == '\\') {
            out[i] = '/';
        }
    }
    return out;
}

static std::string to_lower(const std::string &input) {
    std::string out = input;
    for (size_t i = 0; i < out.size(); i++) {
        out[i] = (char)tolower((unsigned char)out[i]);
    }
    return out;
}

static std::string compute_preferred_prefix(const char *rar_path) {
    if (!rar_path) {
        return std::string();
    }
    const char *base = strrchr(rar_path, '/');
    const char *base2 = strrchr(rar_path, '\\');
    if (base2 && (!base || base2 > base)) {
        base = base2;
    }
    const char *name = base ? base + 1 : rar_path;
    std::string stem(name);
    std::string lower = to_lower(stem);
    if (lower.size() > 4 && lower.rfind(".rar") == lower.size() - 4) {
        stem = stem.substr(0, stem.size() - 4);
    }
    if (stem.empty()) {
        return std::string();
    }
    return stem + "/";
}

static int read_file_to_buffer(const char *path, size_t max_size, char **out_buf, size_t *out_size) {
    if (!path || !out_buf || !out_size) {
        return UNRAR_ERR_READ;
    }
    *out_buf = NULL;
    *out_size = 0;
    struct stat st;
    if (stat(path, &st) != 0) {
        return UNRAR_ERR_READ;
    }
    if (st.st_size <= 0 || (size_t)st.st_size > max_size) {
        return UNRAR_ERR_READ;
    }
    FILE *fp = fopen(path, "rb");
    if (!fp) {
        return UNRAR_ERR_READ;
    }
    size_t size = (size_t)st.st_size;
    char *buf = (char *)malloc(size);
    if (!buf) {
        fclose(fp);
        return UNRAR_ERR_MEMORY;
    }
    size_t read_bytes = fread(buf, 1, size, fp);
    fclose(fp);
    if (read_bytes != size) {
        free(buf);
        return UNRAR_ERR_READ;
    }
    *out_buf = buf;
    *out_size = size;
    return UNRAR_OK;
}

static const RarProbeEntry *find_param_entry(const std::vector<RarProbeEntry> &entries, const std::string &preferred_prefix) {
    std::vector<const RarProbeEntry *> sce_candidates;
    std::vector<const RarProbeEntry *> param_candidates;
    for (size_t i = 0; i < entries.size(); i++) {
        const RarProbeEntry &entry = entries[i];
        if (entry.lower.size() >= strlen("sce_sys/param.json") &&
            (entry.lower.rfind("/sce_sys/param.json") == entry.lower.size() - strlen("/sce_sys/param.json") ||
             entry.lower.rfind("sce_sys/param.json") == entry.lower.size() - strlen("sce_sys/param.json"))) {
            sce_candidates.push_back(&entry);
        } else if (entry.lower == "param.json" ||
                   (entry.lower.size() >= strlen("/param.json") &&
                    entry.lower.rfind("/param.json") == entry.lower.size() - strlen("/param.json"))) {
            param_candidates.push_back(&entry);
        }
    }

    if (!preferred_prefix.empty()) {
        std::string prefix_lower = to_lower(preferred_prefix);
        const RarProbeEntry *best = NULL;
        size_t best_len = 0;
        for (size_t i = 0; i < sce_candidates.size(); i++) {
            const RarProbeEntry *entry = sce_candidates[i];
            if (entry->lower.rfind(prefix_lower, 0) == 0) {
                if (!best || entry->normalized.size() < best_len) {
                    best = entry;
                    best_len = entry->normalized.size();
                }
            }
        }
        if (!best) {
            for (size_t i = 0; i < param_candidates.size(); i++) {
                const RarProbeEntry *entry = param_candidates[i];
                if (entry->lower.rfind(prefix_lower, 0) == 0) {
                    if (!best || entry->normalized.size() < best_len) {
                        best = entry;
                        best_len = entry->normalized.size();
                    }
                }
            }
        }
        if (best) {
            return best;
        }
    }

    if (!sce_candidates.empty()) {
        const RarProbeEntry *best = sce_candidates[0];
        for (size_t i = 1; i < sce_candidates.size(); i++) {
            if (sce_candidates[i]->normalized.size() < best->normalized.size()) {
                best = sce_candidates[i];
            }
        }
        return best;
    }
    if (!param_candidates.empty()) {
        const RarProbeEntry *best = param_candidates[0];
        for (size_t i = 1; i < param_candidates.size(); i++) {
            if (param_candidates[i]->normalized.size() < best->normalized.size()) {
                best = param_candidates[i];
            }
        }
        return best;
    }
    return NULL;
}

static const RarProbeEntry *find_entry_by_normalized(const std::vector<RarProbeEntry> &entries, const std::string &normalized) {
    std::string target = to_lower(normalized);
    for (size_t i = 0; i < entries.size(); i++) {
        if (entries[i].lower == target) {
            return &entries[i];
        }
    }
    return NULL;
}

static int extract_entry_to_path(const char *rar_path, const RarProbeEntry *entry, const char *dest_dir, const char *dest_name) {
    if (!rar_path || !entry || !dest_dir || !dest_name) {
        return UNRAR_ERR_READ;
    }
    struct RAROpenArchiveData arc_data;
    memset(&arc_data, 0, sizeof(arc_data));
    arc_data.ArcName = (char *)rar_path;
    arc_data.OpenMode = RAR_OM_EXTRACT;

    HANDLE hArc = RAROpenArchive(&arc_data);
    if (!hArc || arc_data.OpenResult != ERAR_SUCCESS) {
        return UNRAR_ERR_OPEN;
    }

    struct RARHeaderDataEx header;
    int result = UNRAR_ERR_READ;
    while (1) {
        memset(&header, 0, sizeof(header));
        int read_result = RARReadHeaderEx(hArc, &header);
        if (read_result == ERAR_END_ARCHIVE) {
            break;
        }
        if (read_result != ERAR_SUCCESS) {
            break;
        }
        std::string normalized = normalize_path(header.FileName);
        std::string lower = to_lower(normalized);
        if (lower == entry->lower) {
            char dest_path_buf[PATH_MAX];
            char dest_name_buf[PATH_MAX];
            snprintf(dest_path_buf, sizeof(dest_path_buf), "%s", dest_dir);
            snprintf(dest_name_buf, sizeof(dest_name_buf), "%s", dest_name);
            int proc_result = RARProcessFile(hArc, RAR_EXTRACT, dest_path_buf, dest_name_buf);
            result = (proc_result == ERAR_SUCCESS) ? UNRAR_OK : UNRAR_ERR_EXTRACT;
            break;
        } else {
            RARProcessFile(hArc, RAR_SKIP, NULL, NULL);
        }
    }
    RARCloseArchive(hArc);
    return result;
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

extern "C" int unrar_probe_archive(const char *rar_path,
                                   char **param_buf, size_t *param_size,
                                   char **cover_buf, size_t *cover_size) {
    if (!param_buf || !param_size || !cover_buf || !cover_size) {
        return UNRAR_ERR_READ;
    }
    *param_buf = NULL;
    *param_size = 0;
    *cover_buf = NULL;
    *cover_size = 0;

    if (!rar_path || !*rar_path) {
        return UNRAR_ERR_READ;
    }

    struct RAROpenArchiveData arc_data;
    memset(&arc_data, 0, sizeof(arc_data));
    arc_data.ArcName = (char *)rar_path;
    arc_data.OpenMode = RAR_OM_LIST;

    HANDLE hArc = RAROpenArchive(&arc_data);
    if (!hArc || arc_data.OpenResult != ERAR_SUCCESS) {
        return UNRAR_ERR_OPEN;
    }

    std::vector<RarProbeEntry> entries;
    struct RARHeaderDataEx header;
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
        if ((header.Flags & RHDF_DIRECTORY) == 0) {
            std::string original(header.FileName);
            std::string normalized = normalize_path(original);
            RarProbeEntry entry;
            entry.original = original;
            entry.normalized = normalized;
            entry.lower = to_lower(normalized);
            entries.push_back(entry);
        }
        RARProcessFile(hArc, RAR_SKIP, NULL, NULL);
    }
    RARCloseArchive(hArc);

    if (entries.empty()) {
        return UNRAR_OK;
    }

    std::string preferred_prefix = compute_preferred_prefix(rar_path);
    const RarProbeEntry *param_entry = find_param_entry(entries, preferred_prefix);
    if (!param_entry) {
        return UNRAR_OK;
    }

    std::string prefix;
    size_t sce_pos = param_entry->lower.rfind("sce_sys/param.json");
    if (sce_pos != std::string::npos) {
        prefix = param_entry->normalized.substr(0, sce_pos);
    } else {
        size_t param_pos = param_entry->lower.rfind("param.json");
        if (param_pos != std::string::npos) {
            prefix = param_entry->normalized.substr(0, param_pos);
        }
    }
    if (!prefix.empty() && prefix[prefix.size() - 1] != '/') {
        prefix.push_back('/');
    }

    const char *candidates[] = {
        "icon0.png",
        "icon0.jpg",
        "icon0.jpeg",
        "icon.png",
        "cover.png",
        "cover.jpg",
        "tile0.png",
    };
    const size_t candidate_count = sizeof(candidates) / sizeof(candidates[0]);

    const RarProbeEntry *cover_entry = NULL;
    for (size_t i = 0; i < candidate_count && !cover_entry; i++) {
        std::string candidate = prefix + "sce_sys/" + candidates[i];
        cover_entry = find_entry_by_normalized(entries, candidate);
    }
    for (size_t i = 0; i < candidate_count && !cover_entry; i++) {
        std::string candidate = prefix + candidates[i];
        cover_entry = find_entry_by_normalized(entries, candidate);
    }

    const char *probe_root = "/data/ps5upload/temp";
    if (mkdir(probe_root, 0777) != 0 && errno != EEXIST) {
        return UNRAR_ERR_READ;
    }
    char temp_dir[] = "/data/ps5upload/temp/probe_XXXXXX";
    if (!mkdtemp(temp_dir)) {
        return UNRAR_ERR_READ;
    }

    char param_path[PATH_MAX];
    char cover_path[PATH_MAX];
    snprintf(param_path, sizeof(param_path), "%s/param.json", temp_dir);
    snprintf(cover_path, sizeof(cover_path), "%s/cover.bin", temp_dir);

    int result = UNRAR_OK;
    if (param_entry) {
        int extract_result = extract_entry_to_path(rar_path, param_entry, temp_dir, "param.json");
        if (extract_result == UNRAR_OK) {
            int read_result = read_file_to_buffer(param_path, UNRAR_PROBE_PARAM_MAX, param_buf, param_size);
            if (read_result != UNRAR_OK) {
                *param_buf = NULL;
                *param_size = 0;
            }
        }
    }
    if (cover_entry) {
        int extract_result = extract_entry_to_path(rar_path, cover_entry, temp_dir, "cover.bin");
        if (extract_result == UNRAR_OK) {
            int read_result = read_file_to_buffer(cover_path, UNRAR_PROBE_COVER_MAX, cover_buf, cover_size);
            if (read_result != UNRAR_OK) {
                *cover_buf = NULL;
                *cover_size = 0;
            }
        }
    }

    unlink(param_path);
    unlink(cover_path);
    rmdir(temp_dir);

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
                        snprintf(first_root, sizeof(first_root), "%s", current_root);
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
