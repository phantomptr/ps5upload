/* Direct file reception */

#ifndef EXTRACT_H
#define EXTRACT_H

#include <sys/cdefs.h>

__BEGIN_DECLS

// Spawns a detached thread to handle archive extraction.
// Returns 0 on success (socket ownership is transferred to the new thread).
// Returns -1 on failure (e.g., out of memory, thread creation failed).
int start_threaded_extraction(int client_sock, const char *src, const char *dst);

// Receive folder stream directly from socket (no compression/tar)
// Returns 0 on success, -1 on failure
// Sets out_total_bytes and out_file_count if provided (can be NULL)
int receive_folder_stream(int sock, const char *dest_path, char *err, size_t err_len,
                          unsigned long long *out_total_bytes, int *out_file_count);

__END_DECLS

#endif /* EXTRACT_H */
