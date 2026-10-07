#ifndef PS5UPLOAD_SCE_APP_INST_UTIL_H
#define PS5UPLOAD_SCE_APP_INST_UTIL_H

/* AppInstUtil ABI used by the PS5Upload installer daemon. Struct layouts are
 * the documented Sony AppInstUtil ABI. PlayGoInfo's trailing pad is 6480
 * bytes. This header carries no code from any third party. */

#include <stdint.h>

#define PLAYGOSCENARIOID_SIZE 3
#define CONTENTID_SIZE        0x30
#define LANGUAGE_SIZE         8
#define SCE_NUM_LANGUAGES     30
#define SCE_NUM_IDS           64

typedef char playgo_scenario_id_t[PLAYGOSCENARIOID_SIZE];
typedef char language_t[LANGUAGE_SIZE];
typedef char content_id_t[CONTENTID_SIZE];

typedef struct {
    content_id_t content_id;
    int          content_type;
    int          content_platform;
} SceAppInstallPkgInfo;

/* The six pointers are the layout everyone publishes, and it is too short: Sony reads the
 * 8 bytes that follow icon_url. Measured on a CFI-1115A (FW 13.60) with the same package,
 * the same link and nothing else changed between calls:
 *   those 8 bytes zero        -> a plain install (an installed copy is uninstalled first)
 *   those 8 bytes 0xAA...     -> the patch path (DbgCancelPatch), refused 0x80B2116F
 *   the next 40 bytes 0xAA... -> no effect
 * With a bare six-pointer struct on the stack they were whatever an earlier call left
 * there, which is what "the console refuses its own copy" and every other intermittent
 * 0x80B2116F turned out to be. What the field means is not known (1 was accepted, and
 * skipped the uninstall); a plain install wants 0. The rest is zeroed room in case a
 * firmware reads further. */
typedef struct {
    const char *uri;
    const char *ex_uri;
    const char *playgo_scenario_id;
    const char *content_id;
    const char *content_name;
    const char *icon_url;
    uint64_t    unknown_30;     /* read by Sony; must be 0 for a plain install */
    uint64_t    reserved[7];    /* keep zero */
} MetaInfo;
_Static_assert(sizeof(MetaInfo) == 0x70, "MetaInfo must carry the zeroed tail Sony reads");

typedef struct {
    language_t           languages[SCE_NUM_LANGUAGES];
    playgo_scenario_id_t playgo_scenario_ids[SCE_NUM_IDS];
    content_id_t         content_ids[SCE_NUM_IDS];
    unsigned char        unknown[6480];
} PlayGoInfo;

extern int sceAppInstUtilInitialize(void);
extern int sceAppInstUtilInstallByPackage(MetaInfo *meta,
                                          SceAppInstallPkgInfo *pkg_info,
                                          PlayGoInfo *playgo);

/* SCE_APP_INSTALLER_ERROR_* (facility 0x80A3). */
#define SCE_APP_INSTALLER_ERROR_NOSPACE                  0x80A30002u
#define SCE_APP_INSTALLER_ERROR_APP_NOT_FOUND            0x80A30004u
#define SCE_APP_INSTALLER_ERROR_APP_BROKEN               0x80A30008u
#define SCE_APP_INSTALLER_ERROR_PKG_INVALID_CONTENT_TYPE 0x80A30009u
#define SCE_APP_INSTALLER_ERROR_USED_APP_NOT_FOUND       0x80A3000Au
#define SCE_APP_INSTALLER_ERROR_APP_IS_RUNNING           0x80A3000Cu
#define SCE_APP_INSTALLER_ERROR_SYSTEM_VERSION           0x80A3000Du
#define SCE_APP_INSTALLER_ERROR_CONTENT_ID_DISAGREE      0x80A3000Fu
#define SCE_APP_INSTALLER_ERROR_APP_VER                  0x80A30011u
#define SCE_APP_INSTALLER_ERROR_NEED_ADDCONT_INSTALL     0x80A30017u
#define SCE_APP_INSTALLER_ERROR_INVALID_PATCH_PKG        0x80A30019u

#endif /* PS5UPLOAD_SCE_APP_INST_UTIL_H */
