#ifndef PS5UPLOAD_INSTALLER_HINTS_H
#define PS5UPLOAD_INSTALLER_HINTS_H

#include <stdint.h>

/* A short, reworded, branding-free hint for the install errors people
 * actually hit (patches most of all), or NULL for an unknown code. */
const char *inst_install_hint(uint32_t code);

#endif /* PS5UPLOAD_INSTALLER_HINTS_H */
