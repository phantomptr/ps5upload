/* PS5's loader does not expose FreeBSD's rtld module enumeration API.
 * Report the unsupported operation explicitly. The backtrace crate may still
 * capture addresses, but its optional ELF/DWARF symbolization has no modules.
 * This does not fabricate a successful enumeration or affect CLI operations.
 */
#include <errno.h>
#include <link.h>

int dl_iterate_phdr(int (*callback)(struct dl_phdr_info *, size_t, void *), void *data) {
    (void)callback;
    (void)data;
    errno = ENOSYS;
    return -1;
}
