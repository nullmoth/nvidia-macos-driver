/* nm_dmc_push <file>: hands Intel display firmware to the hybrid-laptop panel driver (debug.nmintelfb_dmc). Root only. The driver
 * parses and range-checks the bytes itself; this tool only reads the file and writes it to the sysctl. Built with:
 *   cc -O2 -o nm_dmc_push nm_dmc_push.c */
#include <stdio.h>
#include <stdlib.h>
#include <sys/sysctl.h>
int main(int argc, char **argv)
{
    static unsigned char buf[160 * 1024 + 1];
    FILE *f; size_t n;
    if (argc != 2) { fprintf(stderr, "usage: nm_dmc_push <firmware file>\n"); return 2; }
    f = fopen(argv[1], "rb");
    if (!f) { perror(argv[1]); return 1; }
    n = fread(buf, 1, sizeof buf, f); fclose(f);
    if (n < 1024 || n > 160 * 1024) { fprintf(stderr, "%s: %zu bytes is not a display firmware file\n", argv[1], n); return 1; }
    if (sysctlbyname("debug.nmintelfb_dmc", NULL, NULL, buf, n) != 0) { perror("debug.nmintelfb_dmc"); return 1; }
    printf("display firmware %s handed to the panel driver (%zu bytes)\n", argv[1], n);
    return 0;
}
