#ifndef PS5UPLOAD_FAKELIB_OVERLAY_H
#define PS5UPLOAD_FAKELIB_OVERLAY_H

#include <stddef.h>

int fakelib_overlay_start(void);
void fakelib_overlay_stop(void);
int fakelib_overlay_status_json(char *buf, size_t cap);

#endif
