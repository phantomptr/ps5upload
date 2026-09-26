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

typedef struct {
    const char *uri;
    const char *ex_uri;
    const char *playgo_scenario_id;
    const char *content_id;
    const char *content_name;
    const char *icon_url;
} MetaInfo;

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
