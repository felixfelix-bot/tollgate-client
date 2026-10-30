/* Linux process-crash observation shim, NOT a storage fault simulator.
 * Forward the real sync first, then stop at the selected boundary. No SQLite
 * return values, writes, locking or durability settings are changed. */
#define _GNU_SOURCE
#include <dlfcn.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

static void checkpoint(int fd) {
    const char *target = getenv("BUDGET_SYNC_TARGET");
    if (!target) return;
    char link[64], path[4096];
    snprintf(link, sizeof(link), "/proc/self/fd/%d", fd);
    ssize_t n = readlink(link, path, sizeof(path) - 1);
    if (n < 0) return;
    path[n] = 0;
    if (strcmp(path, target)) return;
    if (write(STDOUT_FILENO, "CRASH_READY\n", 12) != 12) _exit(91);
    char byte;
    /* Parent kills us here; EOF/release is an error, never continued execution. */
    (void)read(STDIN_FILENO, &byte, 1);
    _exit(92);
}
int fsync(int fd) {
    int (*real_sync)(int) = dlsym(RTLD_NEXT, "fsync");
    if (!real_sync) _exit(93);
    int result = real_sync(fd);
    if (result == 0) checkpoint(fd);
    return result;
}
int fdatasync(int fd) {
    int (*real_sync)(int) = dlsym(RTLD_NEXT, "fdatasync");
    if (!real_sync) _exit(93);
    int result = real_sync(fd);
    if (result == 0) checkpoint(fd);
    return result;
}
