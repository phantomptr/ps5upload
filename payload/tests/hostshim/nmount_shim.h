/* Host builds: FreeBSD's nmount(2), which macOS lacks (the shell's mtrw uses it; the tests stub it). */
#include <sys/uio.h>
int nmount(struct iovec *iov, unsigned niov, int flags);
