#ifndef PS5UPLOAD_INSTALLER_THREAD_NAME_H
#define PS5UPLOAD_INSTALLER_THREAD_NAME_H

/* Names for the installer daemon's threads. Every thread entry point names
 * itself first: SYS_thr_set_name names only the CALLING thread, and the
 * process list shows whichever thread the kernel treats as representative,
 * so naming main alone is not enough.
 *
 * None of these may start with "ps5upload": the helper's own-instance sweep
 * SIGKILLs that prefix (proc_identity.h), and the installer is a separate
 * process it must leave alone. None may be "payload.elf" either: other
 * loaders kill that name. All stay under the kernel's 19-character thread
 * name limit. Copied from the helper's proc_name_set_self rather than
 * included from ../include, so the two builds stay independent. */
#include <unistd.h>
#include <sys/syscall.h>

#define INST_THREAD_MAIN      "ps5u-installer"
#define INST_THREAD_MONITOR   "ps5u-inst-monitor"
#define INST_THREAD_CONN      "ps5u-inst-conn"
#define INST_THREAD_INIT      "ps5u-inst-init"
#define INST_THREAD_LB_LISTEN "ps5u-inst-lb"
#define INST_THREAD_LB_WORKER "ps5u-inst-lbconn"

#ifdef SYS_thr_set_name
static inline void inst_thread_name(const char *name) {
    (void)syscall(SYS_thr_set_name, -1, name);
}
#else
/* Host builds (the selftests) have no SYS_thr_set_name. */
static inline void inst_thread_name(const char *name) {
    (void)name;
}
#endif

#endif /* PS5UPLOAD_INSTALLER_THREAD_NAME_H */
