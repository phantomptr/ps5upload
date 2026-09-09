#ifndef PS5UPLOAD_SDK_CHANGER_H
#define PS5UPLOAD_SDK_CHANGER_H

#include <stddef.h>

void sdk_changer_init(void);

int sdk_changer_scan(char *buf, size_t cap, size_t *written);

/* `patch_libc` applies BestPig's libc.prx symbol swap. OFF by default and
 * deliberately not automatic: it is documented as helping SOME titles, and on
 * the test console applying it to a title that did not need it crashed the
 * game after five modules instead of loading seventy. */
int sdk_changer_patch(const char *title_id, const char *target_sdk,
                      int patch_libc, char *err, size_t err_cap);

int sdk_changer_restore(const char *title_id, int *restored_count,
                        char *err, size_t err_cap);

#endif
