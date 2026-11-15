/* Native FreeBSD devfs terminal adapters. Kernel errors remain visible. */
#include <errno.h>
#include <fcntl.h>
#include <limits.h>
#include <stdlib.h>
#include <string.h>
#include <sys/filio.h>
#include <sys/ioctl.h>
#include <termios.h>
#include <unistd.h>

static int device_path(int fd, char *buffer, size_t size) {
    if (size <= 5 || size - 5 > INT_MAX) return ERANGE;
    memcpy(buffer, "/dev/", 5);
    buffer[5] = '\0';
    struct fiodgname_arg request = {(int)(size - 5), buffer + 5};
    if (ioctl(fd, FIODGNAME, &request) < 0) return errno;
    if (memchr(buffer + 5, '\0', size - 5) == NULL) return ERANGE;
    return 0;
}

int ttyname_r(int fd, char *buffer, size_t size) {
    struct termios attributes;
    if (tcgetattr(fd, &attributes) < 0) return errno;
    return device_path(fd, buffer, size);
}

int openpty(int *master_out, int *slave_out, char *name,
            struct termios *attributes, struct winsize *window) {
    int master = posix_openpt(O_RDWR | O_NOCTTY);
    if (master < 0) return -1;
    /* FreeBSD's posix_openpt allocates and grants the slave in the kernel.
     * TIOCPTMASTER verifies that this descriptor is a Unix98 PTY master.
     */
    int slave = -1;
    char path[PATH_MAX];
    if (ioctl(master, TIOCPTMASTER) < 0) goto fail;
    int error = device_path(master, path, sizeof(path));
    if (error) { errno = error; goto fail; }
    slave = open(path, O_RDWR | O_NOCTTY);
    if (slave < 0) goto fail;
    if (attributes && tcsetattr(slave, TCSAFLUSH, attributes) < 0) goto fail;
    if (window && ioctl(slave, TIOCSWINSZ, window) < 0) goto fail;
    if (name) strcpy(name, path); /* openpty's API gives no buffer length. */
    *master_out = master;
    *slave_out = slave;
    return 0;
fail:
    error = errno;
    if (slave >= 0) close(slave);
    close(master);
    errno = error;
    return -1;
}
