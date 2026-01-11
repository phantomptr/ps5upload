/* Storage detection and listing */

#ifndef STORAGE_H
#define STORAGE_H

#include <sys/cdefs.h>

__BEGIN_DECLS

void handle_list_storage(int client_sock);
void handle_list_dir(int client_sock, const char *path);

__END_DECLS

#endif /* STORAGE_H */
