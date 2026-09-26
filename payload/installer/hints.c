#include "hints.h"

#include <stddef.h>  /* NULL */

/* SCE_APP_INSTALLER_ERROR_* values (see payload/include/sceAppInstUtil.h).
 * Duplicated as literals here so hints.c stays pure and host-buildable
 * without pulling in the Sony header. */
const char *inst_install_hint(uint32_t code) {
    switch (code) {
        case 0x80A30004u: /* APP_NOT_FOUND */
        case 0x80A3000Au: /* USED_APP_NOT_FOUND */
            return "install the base game first";
        case 0x80A3000Fu: /* CONTENT_ID_DISAGREE */
            return "this patch is for a different game";
        case 0x80A30011u: /* APP_VER */
            return "the base game is the wrong version for this patch";
        case 0x80A30019u: /* INVALID_PATCH_PKG */
            return "the patch package is invalid or corrupt";
        case 0x80A30017u: /* NEED_ADDCONT_INSTALL */
            return "this DLC needs its base content installed";
        case 0x80A3000Du: /* SYSTEM_VERSION */
            return "the console firmware is too old for this package";
        case 0x80A3000Cu: /* APP_IS_RUNNING */
            return "close the game before installing";
        case 0x80A30002u: /* NOSPACE */
            return "not enough free space";
        case 0x80A30008u: /* APP_BROKEN */
        case 0x80A30009u: /* PKG_INVALID_CONTENT_TYPE */
            return "the package is broken or the wrong type";
        default:
            return NULL;
    }
}
