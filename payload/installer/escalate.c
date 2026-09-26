#include "escalate.h"

#include <string.h>
#include <stdint.h>
#include <unistd.h>
#include <sys/types.h>

#include <ps5/kernel.h>
#include "authid.h"   /* PS5_JB_AUTHID */

int inst_escalate_self(void) {
    pid_t pid = getpid();
    intptr_t proc = kernel_get_proc(pid);
    if (!proc) return -1;

    int rc = 0;
    if (kernel_set_ucred_uid  (pid, 0) != 0) rc = -1;
    if (kernel_set_ucred_ruid (pid, 0) != 0) rc = -1;
    if (kernel_set_ucred_svuid(pid, 0) != 0) rc = -1;
    if (kernel_set_ucred_rgid (pid, 0) != 0) rc = -1;
    if (kernel_set_ucred_svgid(pid, 0) != 0) rc = -1;

    intptr_t rootvnode = kernel_get_root_vnode();
    if (rootvnode) {
        if (kernel_set_proc_rootdir(pid, rootvnode) != 0) rc = -1;
        if (kernel_set_proc_jaildir(pid, rootvnode) != 0) rc = -1;
    }

    /* SYSTEM authid — BGFT's task-creation gate for http:// installs. */
    if (kernel_set_ucred_authid(pid, PS5_JB_AUTHID) != 0) rc = -1;

    uint8_t caps[16];
    memset(caps, 0xff, sizeof(caps));
    if (kernel_set_ucred_caps(pid, caps) != 0) rc = -1;

    uint8_t attrs[32];
    memset(attrs, 0, sizeof(attrs));
    attrs[0] = 0x80;
    if (kernel_set_ucred_attrs(pid, attrs) != 0) rc = -1;

    return rc;
}
