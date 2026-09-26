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

/* One-time initialization of the AppInstUtil/IPMI backend. Must be
 * called at least once before InstallByPackage/AppInstallPkg; calling
 * InstallByPackage cold (without init) leaves IPMI half-wedged and
 * trips Sony's watchdog a few seconds later. The daemon wraps this in
 * a timed_init with a 10s timeout + per-request retry. */
extern int sceAppInstUtilInitialize(void);

/* HTTP-URL installer (ezremote-dpi path): parses meta->uri as a URI and
 * fetches over HTTP. Gated by Sony's PlayGo HTTP pre-flight on some FW
 * (0x80B22404), so it's the secondary path. */
extern int sceAppInstUtilInstallByPackage(MetaInfo *meta,
                                          SceAppInstallPkgInfo *pkg_info,
                                          PlayGoInfo *playgo);

/* Local-disk installer (elf-arsenal path): takes a bare absolute path to a
 * .pkg already on the console's disk and installs it with no URI parse and
 * no HTTP pre-flight. This is the path that works without the PlayGo gate;
 * pkg_info.content_id is filled from the pkg header on success. */
extern int sceAppInstUtilAppInstallPkg(const char *path,
                                       SceAppInstallPkgInfo *pkg_info);

/* Async install status, keyed by content_id.
 *
 * InstallByPackage returns as soon as the task is *queued* (rc == 0). The
 * real outcome — especially a patch validation failure — arrives later on
 * this channel. Only the process that started the install may poll it (Sony
 * segfaults a cross-process poller), which is why polling lives here in the
 * DPI daemon and not in the main payload. Layout is the documented Sony ABI
 * (cf. OnionHEN's PS5 install writeup). */
typedef struct {
    int32_t error_code;
    int32_t version;
    char    description[512];
    char    type[9];
} SceAppInstallErrorInfo;

typedef struct {
    char                   status[16];  /* "installing" | "playable" | "error" | "none" */
    char                   src_type[8];
    uint32_t               remain_time;
    uint64_t               downloaded_size;
    uint64_t               initial_chunk_size;
    uint64_t               total_size;
    uint32_t               promote_progress;
    SceAppInstallErrorInfo error_info;
    int32_t                local_copy_percent;
    unsigned char          is_copy_only;
} SceAppInstallStatusInstalled;

extern int sceAppInstUtilGetInstallStatus(const char *content_id,
                                          SceAppInstallStatusInstalled *status);

/* SCE_APP_INSTALLER_ERROR_* — ShellUI's values. Base UNKNOWN = 0x80A30001;
 * PARAM = 0x80A30003 matches ps5upload's own hardware-observed code, which is
 * how the facility (0x80A3, not 0x80A2) was pinned. The patch-relevant ones
 * turn "patch mostly fails" into a real reason. */
#define SCE_APP_INSTALLER_ERROR_NOSPACE             0x80A30002u
#define SCE_APP_INSTALLER_ERROR_APP_NOT_FOUND       0x80A30004u /* base not installed */
#define SCE_APP_INSTALLER_ERROR_APP_BROKEN          0x80A30008u
#define SCE_APP_INSTALLER_ERROR_PKG_INVALID_CONTENT_TYPE 0x80A30009u
#define SCE_APP_INSTALLER_ERROR_USED_APP_NOT_FOUND  0x80A3000Au
#define SCE_APP_INSTALLER_ERROR_APP_IS_RUNNING      0x80A3000Cu /* close the game first */
#define SCE_APP_INSTALLER_ERROR_SYSTEM_VERSION      0x80A3000Du /* patch needs newer FW */
#define SCE_APP_INSTALLER_ERROR_CONTENT_ID_DISAGREE 0x80A3000Fu /* patch != base */
#define SCE_APP_INSTALLER_ERROR_APP_VER             0x80A30011u /* base is the wrong version */
#define SCE_APP_INSTALLER_ERROR_NEED_ADDCONT_INSTALL 0x80A30017u
#define SCE_APP_INSTALLER_ERROR_INVALID_PATCH_PKG   0x80A30019u

#endif /* PS5UPLOAD_DPI_SCE_APP_INST_UTIL_H */
