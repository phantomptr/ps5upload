#ifndef PS5UPLOAD2_SONY_API_LOCK_H
#define PS5UPLOAD2_SONY_API_LOCK_H

/*
 * Shared mutex serializing payload-process calls into Sony's
 * installer / launch / uninstall kernel stubs.
 *
 * Sony's kernel stubs (sceAppInstUtilInstallTitleDir,
 * sceAppInstUtilInstallByPackage, sceAppInstUtilGetInstallStatus,
 * sceLncUtilLaunchApp, sceSystemServiceLaunchApp,
 * sceAppInstUtilAppUnInstall) appear to hold internal locks briefly
 * on return on FW 9.60+. Concurrent calls from multiple mgmt
 * handler threads can hit the stubs while those locks are still
 * held, deadlocking the calling thread. This lock forces
 * one-at-a-time access regardless of which file the call site lives
 * in.
 *
 * The 200 µs post-call grace period (`SONY_API_POST_SLEEP_US`) lets
 * Sony's kernel stub release its internal locks before the next
 * caller enters; callers should sleep this duration *while still
 * holding the lock*, then unlock.
 *
 * Originally lived as a static `g_sony_api_mtx` inside register.c.
 * That left bgft.c's in-process AppInstUtil install/status paths
 * racing with register.c's title-install / launch / uninstall — a
 * concurrent register from one mgmt thread plus a `pkg install
 * status` poll from another could land in Sony's installer kernel
 * stubs simultaneously and trigger the documented deadlock.
 * Promoted to a shared extern in 2.2.61 after a multi-pass audit
 * surfaced the cross-mutex split.
 */

#include <pthread.h>

extern pthread_mutex_t sony_api_lock;

/* Post-call grace period — empirically tuned. usleep is FreeBSD-
 * native on the PS5 SDK so no dlsym dance. Caller pattern:
 *
 *   pthread_mutex_lock(&sony_api_lock);
 *   ... call sce* ...
 *   usleep(SONY_API_POST_SLEEP_US);
 *   pthread_mutex_unlock(&sony_api_lock);
 */
#define SONY_API_POST_SLEEP_US 200000u

#endif /* PS5UPLOAD2_SONY_API_LOCK_H */
