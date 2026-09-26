#ifndef PS5UPLOAD_FTP_SERVER_H
#define PS5UPLOAD_FTP_SERVER_H

#include <stddef.h>
#include <stdint.h>

void ftp_server_init(void);

int ftp_server_start(int port, const char *root, int readonly,
                     const char *user, const char *pass,
                     char *resp, size_t cap, size_t *written);

int ftp_server_stop(char *resp, size_t cap, size_t *written);

int ftp_server_status(char *resp, size_t cap, size_t *written);

#endif
