#ifndef PS5UPLOAD2_CONFIG_H
#define PS5UPLOAD2_CONFIG_H

/* Compile-time version reported in the STATUS_ACK body. Lets the desktop
 * UI tell apart an old payload still running from a build that includes
 * a particular fix, without having to boot the console. Keep in sync
 * with the desktop app's package.json during releases. */
#define PS5UPLOAD2_VERSION "6.3.1"
/* Author credit — embedded in the startup toast so anyone looking at
 * the console screen knows who wrote the software that just loaded.
 * Kept separate from VERSION so release scripts can bump the version
 * without touching this line. */
#define PS5UPLOAD2_AUTHOR "PhantomPtr"

#define PS5UPLOAD2_RUNTIME_ROOT "/data/ps5upload"
#define PS5UPLOAD2_RUNTIME_DIR "/data/ps5upload/runtime"
#define PS5UPLOAD2_DEBUG_DIR "/data/ps5upload/debug"
/* 2.2.52 Tier-1 install staging. 2.2.54-fix-round-15 moved this to
 * /user/data/... after empirical testing showed Sony path-allowlists
 * the URI argument to sceAppInstUtilInstallByPackage:
 *   /user/data/  -- accepted
 *   /mnt/usb / --  accepted (Sony debug menu uses this)
 *   /data/       -- rejected with 0x80B2_116F (parser group) or
 *                   0x80B2_150F (install-state group)
 * Confirmed via curl test on the user 41 MB Store pkg: stage to /data
 * register fails; stage to /user/data register accepts (err_code=0). */
#define PS5UPLOAD2_PKG_TEMP_DIR "/user/data/ps5upload/pkg_temp"
/* Parent dir for the staging tree -- created by runtime_ensure_directories. */
#define PS5UPLOAD2_USER_DATA_ROOT "/user/data/ps5upload"
/* Per-mount tracking files (name.src containing the source image
 * path). Used by FS_LIST_VOLUMES to surface which .exfat/.ffpkg
 * backs each /mnt/ps5upload/<name> mount point, and by payload
 * startup reconciliation to detect orphaned mounts. */
#define PS5UPLOAD2_MOUNTS_DIR "/data/ps5upload/mounts"

#endif
