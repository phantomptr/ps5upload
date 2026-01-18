/* C wrapper for unrar library
 * Provides simple extraction API callable from C code
 */

#ifndef UNRAR_WRAPPER_H
#define UNRAR_WRAPPER_H

#ifdef __cplusplus
extern "C" {
#endif

/* Error codes */
#define UNRAR_OK              0
#define UNRAR_ERR_OPEN       -1
#define UNRAR_ERR_READ       -2
#define UNRAR_ERR_EXTRACT    -3
#define UNRAR_ERR_PASSWORD   -4
#define UNRAR_ERR_MEMORY     -5
#define UNRAR_ERR_BADARCHIVE -6

/* Callback for progress reporting
 * filename: current file being extracted
 * file_size: size of current file
 * files_done: number of files extracted so far
 * Returns 0 to continue, non-zero to abort
 */
typedef int (*unrar_progress_cb)(const char *filename, unsigned long long file_size, int files_done, void *user_data);

/* Extract a RAR archive to a destination directory
 *
 * rar_path: path to the RAR archive file
 * dest_dir: destination directory for extraction
 * strip_root: whether to strip the top-level directory
 * progress: optional progress callback (can be NULL)
 * user_data: user data passed to callback
 *
 * Returns: UNRAR_OK on success, error code on failure
 */
int unrar_extract(const char *rar_path, const char *dest_dir, int strip_root, unrar_progress_cb progress, void *user_data);

/* Get file count and total uncompressed size from a RAR archive
 *
 * rar_path: path to the RAR archive file
 * file_count: output for number of files
 * total_size: output for total uncompressed size
 * common_root: output buffer for common root directory (optional, pass NULL if not needed)
 * root_len: size of common_root buffer
 *
 * Returns: UNRAR_OK on success, error code on failure
 */
int unrar_scan(const char *rar_path, int *file_count, unsigned long long *total_size, char *common_root, size_t root_len);

/* Get error description string */
const char *unrar_strerror(int err);

#ifdef __cplusplus
}
#endif

#endif /* UNRAR_WRAPPER_H */
