/*
 * Android openpty polyfill
 * Android doesn't have openpty() (BSD), but has posix_openpt/grantpt/unlockpt
 * This provides openpty() implementation for portable-pty on Android
 */

#include <stdlib.h>
#include <unistd.h>
#include <fcntl.h>
#include <termios.h>
#include <sys/ioctl.h>
#include <errno.h>
#include <string.h>

/* Android uses /dev/ptmx for PTY allocation */
int openpty(int *amaster, int *aslave, char *name, const struct termios *termp, const struct winsize *winp) {
    int master_fd = posix_openpt(O_RDWR | O_NOCTTY);
    if (master_fd < 0) {
        return -1;
    }

    if (grantpt(master_fd) < 0) {
        close(master_fd);
        return -1;
    }

    if (unlockpt(master_fd) < 0) {
        close(master_fd);
        return -1;
    }

    char *slave_name = ptsname(master_fd);
    if (slave_name == NULL) {
        close(master_fd);
        return -1;
    }

    int slave_fd = open(slave_name, O_RDWR | O_NOCTTY);
    if (slave_fd < 0) {
        close(master_fd);
        return -1;
    }

    if (termp != NULL) {
        if (tcsetattr(slave_fd, TCSAFLUSH, termp) < 0) {
            close(master_fd);
            close(slave_fd);
            return -1;
        }
    }

    if (winp != NULL) {
        if (ioctl(slave_fd, TIOCSWINSZ, winp) < 0) {
            close(master_fd);
            close(slave_fd);
            return -1;
        }
    }

    if (name != NULL) {
        strncpy(name, slave_name, 1024);  // TTY name buffer
        name[1023] = '\0';
    }

    *amaster = master_fd;
    *aslave = slave_fd;

    return 0;
}