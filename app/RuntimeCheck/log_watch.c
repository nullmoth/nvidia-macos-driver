#include <sys/types.h>
#include <sys/wait.h>
#include <signal.h>
#include <unistd.h>
#include <time.h>
#include <stdio.h>
#include <string.h>
#include <errno.h>
#include <stdlib.h>

static double now(void) {
    struct timespec t;
    if (clock_gettime(CLOCK_MONOTONIC, &t)) return -1;
    return (double)t.tv_sec + (double)t.tv_nsec / 1e9;
}
int main(int argc, char **argv) {
    if (argc != 3 || getuid() != 0 ||
        strncmp(argv[2], "/private/var/root/1401-logs.", 28)) return 77;
    double started = now();
    if (started < 0) return 74;
    pid_t child = fork();
    if (child < 0) return 74;
    if (!child) {
        if (setsid() < 0) _exit(74);
        execl("/bin/bash", "bash", argv[1], "--collect-logs", argv[2], (char *)0);
        _exit(74);
    }
    struct timespec pause = {0, 100000000};
    int status = 0;
    while (1) {
        pid_t result = waitpid(child, &status, WNOHANG);
        if (result == child) return WIFEXITED(status) ? WEXITSTATUS(status) : 75;
        if (result < 0 && errno != EINTR) return 74;
        double current = now();
        /* every log query in the collector has its own cap (sum about 300 s worst case); 180 s killed the kernel log */
        if (current < 0 || current - started >= 360.0) break;
        nanosleep(&pause, 0);
    }
    kill(-child, SIGTERM);
    double grace = now();
    while (now() >= 0 && now() - grace < 5.0) {
        /* Keep the child unreaped until group cleanup, so its ID cannot be reused. */
        nanosleep(&pause, 0);
    }
    kill(-child, SIGKILL);
    while (waitpid(child, &status, 0) < 0 && errno == EINTR) {}
    fputs("Log collection exceeded its time limit; partial files are retained.\n", stderr);
    return 124;
}
