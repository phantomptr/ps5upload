/* Direct file reception */

#ifndef EXTRACT_H
#define EXTRACT_H

#include <sys/cdefs.h>

__BEGIN_DECLS

// Receive folder stream directly from socket (no compression/tar)
// Returns 0 on success, -1 on failure
// Sets out_total_bytes and out_file_count if provided (can be NULL)
int receive_folder_stream(int sock, const char *dest_path, char *err, size_t err_len,
                          long long *out_total_bytes, int *out_file_count);

__END_DECLS

#endif /* EXTRACT_H */
