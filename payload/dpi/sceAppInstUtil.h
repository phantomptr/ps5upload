/*
 * AppInstUtil ABI for the standalone DPI install daemon.
 *
 * Ported to plain C from cy33hc/ps5-ezremote-dpi (source/sceAppInstUtil.h).
 * Struct layouts are the documented Sony AppInstUtil ABI; PlayGoInfo's
 * trailing pad is 6480 bytes (the reference's `unknown[6480]`, == our
 * payload's `long unknown[810]` — same total size).
 */
#ifndef PS5UPLOAD_DPI_SCE_APP_INST_UTIL_H
#define PS5UPLOAD_DPI_SCE_APP_INST_UTIL_H

#include <stdint.h>

#define PLAYGOSCENARIOID_SIZE 3
#define CONTENTID_SIZE        0x30
#define LANGUAGE_SIZE         8

#define SCE_NUM_LANGUAGES 30
#define SCE_NUM_IDS       64

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

extern int sceAppInstUtilInstallByPackage(MetaInfo *meta,
                                          SceAppInstallPkgInfo *pkg_info,
                                          PlayGoInfo *playgo);

#endif /* PS5UPLOAD_DPI_SCE_APP_INST_UTIL_H */
