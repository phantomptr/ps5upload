/* Start lock and the takeover wait (takeover_flag.c), host build.
 *
 * Exactly one of two helpers starting together holds the lock; the other is told BUSY and touches
 * nothing. A lock left by a process that died (crash, power cut) never stops the next start. The
 * takeover wait ends on the monotonic clock, not after a count of probes. fork() here is the host
 * test only: the helper itself never forks. */
#include "takeover_flag.h"

#include <arpa/inet.h>
#include <fcntl.h>
#include <netinet/in.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/socket.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>

static int failures = 0;

static void check(int ok, const char *label) {
    printf("  %s %s\n", ok ? "PASS" : "FAIL", label);
    if (!ok) failures++;
}

static long long mono_ms(void) {
    struct timespec ts;
    clock_gettime(CLOCK_MONOTONIC, &ts);
    return (long long)ts.tv_sec * 1000LL + ts.tv_nsec / 1000000L;
}

static void write_lock_body(const char *dir, const char *body) {
    char path[300];
    snprintf(path, sizeof path, "%s/start.lock", dir);
    int fd = open(path, O_RDWR | O_CREAT, 0644);
    if (fd < 0) return;
    if (ftruncate(fd, 0) == 0) (void)pwrite(fd, body, strlen(body), 0);
    close(fd);
}

int main(void) {
    char dir[] = "/tmp/ps5u-takeover-lock-XXXXXX";
    if (!mkdtemp(dir)) {
        perror("mkdtemp");
        return 1;
    }

    /* One holder at a time. */
    int pid = -1;
    int a = takeover_start_lock(dir, 120000, &pid);
    check(a >= 0, "the first start holds the lock");
    check(pid == 0, "the holder is told no other pid");
    int b = takeover_start_lock(dir, 120000, &pid);
    check(b == TAKEOVER_START_BUSY, "a second start while the first holds it is told BUSY");
    check(pid == (int)getpid(), "and is told the holder's pid");
    takeover_start_unlock(a);
    int c = takeover_start_lock(dir, 120000, &pid);
    check(c >= 0, "once released, the next start holds it");
    takeover_start_unlock(c);

    /* A file left behind holds nothing: whatever it says, nobody holds the kernel lock. */
    write_lock_body(dir, "4242 1\n");
    c = takeover_start_lock(dir, 120000, &pid);
    check(c >= 0, "a file left by a crash or power cut does not stop the start");
    takeover_start_unlock(c);

    /* A holder that dies: the kernel drops its lock. */
    {
        int gate[2];
        if (pipe(gate) != 0) return 1;
        pid_t child = fork();
        if (child == 0) {
            int fd = takeover_start_lock(dir, 120000, NULL);
            char ok = fd >= 0 ? 'y' : 'n';
            (void)write(gate[1], &ok, 1);
            for (;;) pause();
        }
        char got = 0;
        (void)read(gate[0], &got, 1);
        check(got == 'y', "a child process takes the lock");
        int busy = takeover_start_lock(dir, 120000, &pid);
        check(busy == TAKEOVER_START_BUSY && pid == (int)child, "while it lives, the parent is told BUSY");
        kill(child, SIGKILL);
        waitpid(child, NULL, 0);
        c = takeover_start_lock(dir, 120000, &pid);
        check(c >= 0, "once it is killed, the next start holds the lock");
        takeover_start_unlock(c);
        close(gate[0]);
        close(gate[1]);
    }

    /* A holder that has been starting for longer than any start takes is taken for stuck. */
    a = takeover_start_lock(dir, 120000, NULL);
    {
        char body[64];
        snprintf(body, sizeof body, "%d %lld\n", (int)getpid(), mono_ms() - 200000);
        write_lock_body(dir, body);
    }
    b = takeover_start_lock(dir, 120000, &pid);
    check(b == TAKEOVER_START_NO_LOCK, "a holder starting for over max_age: start without the lock");
    /* Taken this instant, not written yet: busy, never "stuck". */
    write_lock_body(dir, "");
    b = takeover_start_lock(dir, 120000, &pid);
    check(b == TAKEOVER_START_BUSY, "a holder that has not written its age yet: BUSY");
    takeover_start_unlock(a);

    check(takeover_start_lock("/nonexistent-dir-for-takeover-test", 120000, &pid) == TAKEOVER_START_NO_LOCK,
          "no lock file can be made: start without the lock");

    /* The takeover wait ends by the monotonic clock. A listener that never goes away. */
    {
        int s = socket(AF_INET, SOCK_STREAM, 0);
        struct sockaddr_in sa;
        memset(&sa, 0, sizeof sa);
        sa.sin_family = AF_INET;
        sa.sin_addr.s_addr = htonl(INADDR_LOOPBACK);
        sa.sin_port = 0;
        socklen_t sl = sizeof sa;
        int ok = s >= 0 && bind(s, (struct sockaddr *)&sa, sizeof sa) == 0 && listen(s, 64) == 0 &&
                 getsockname(s, (struct sockaddr *)&sa, &sl) == 0;
        check(ok, "a loopback listener stands in for the old helper");
        int ports[1] = {(int)ntohs(sa.sin_port)};
        long long t0 = mono_ms();
        /* 5 x 100 ms = 500 ms budget. */
        int rc = takeover_flag_request(dir, 7, ports, 1, 5, 100000);
        long long took = mono_ms() - t0;
        check(rc == -1, "a port that stays busy: the wait gives up");
        check(took >= 450 && took < 2000, "within the budget on the monotonic clock");
        close(s);
        rc = takeover_flag_request(dir, 7, ports, 1, 5, 100000);
        check(rc == 0, "a port that is free: the wait ends at once");
        takeover_flag_unlink(dir);
    }

    char path[300];
    snprintf(path, sizeof path, "%s/start.lock", dir);
    unlink(path);
    rmdir(dir);
    if (failures) {
        printf("takeover lock self-test: %d failure(s)\n", failures);
        return 1;
    }
    printf("takeover lock self-test: all passed\n");
    return 0;
}
