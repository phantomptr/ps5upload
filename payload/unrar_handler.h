/* RAR extraction handler for PS5 Upload
 * Handles receiving RAR files and extracting them
 */

#ifndef UNRAR_HANDLER_H
#define UNRAR_HANDLER_H

#include <stddef.h>

/* Handle UPLOAD_RAR command
 * Receives a RAR file over the socket and extracts to dest_path
 *
 * Protocol:
 * 1. Client sends: UPLOAD_RAR <dest_path> <file_size>\n
 * 2. Server responds: READY\n
 * 3. Client sends: <file_size> bytes of RAR data
 * 4. Server extracts and responds: SUCCESS <files> <bytes>\n or ERROR: <message>\n
 *
 * sock: client socket
 * args: command arguments (dest_path file_size)
 */
void handle_upload_rar(int sock, const char *args);

/* Receive RAR file data into a temporary file
 * Returns the temp file path on success, NULL on error
 * Caller must free the returned string
 */
char *receive_rar_to_temp(int sock, size_t file_size);

/* Extract a RAR file to destination
 * Returns 0 on success, -1 on error
 * file_count and total_bytes are output parameters
 * strip_root: if 1, strips the top-level directory if it exists
 */
int extract_rar_file(const char *rar_path, const char *dest_dir, int strip_root,
                     int *file_count, unsigned long long *total_bytes);

#endif /* UNRAR_HANDLER_H */
