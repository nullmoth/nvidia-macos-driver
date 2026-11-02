// encloop.c: open, use and close many enc.c sessions in one process (M4, T4.07): finds RM/IOKit resource leaks
// without VideoToolbox in the way.
//   ./encloop [-v] SESSIONS W H
#include "enc.h"
#include <fcntl.h>
#include <mach/mach.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

static int open_fds(void) { int n = 0; for (int fd = 0; fd < 4096; fd++) if (fcntl(fd, F_GETFD) != -1) n++; return n; }
static double rss_mb(void)
{
    mach_task_basic_info_data_t i; mach_msg_type_number_t n = MACH_TASK_BASIC_INFO_COUNT;
    task_info(mach_task_self(), MACH_TASK_BASIC_INFO, (task_info_t)&i, &n);
    return i.resident_size / 1048576.0;
}
static unsigned ports(void)
{
    mach_port_name_array_t names; mach_port_type_array_t types; mach_msg_type_number_t n = 0, nt = 0;
    if (mach_port_names(mach_task_self(), &names, &n, &types, &nt) != KERN_SUCCESS) return 0;
    vm_deallocate(mach_task_self(), (vm_address_t)names, n * sizeof *names); vm_deallocate(mach_task_self(), (vm_address_t)types, nt * sizeof *types);
    return n;
}

int main(int argc, char **argv)
{
    int ai = 1; bool v = false;
    if (argc > 1 && !strcmp(argv[1], "-v")) { v = true; ai++; }
    if (argc - ai < 3) { fprintf(stderr, "usage: encloop [-v] SESSIONS W H\n"); return 2; }
    int S = atoi(argv[ai]), W = atoi(argv[ai + 1]), H = atoi(argv[ai + 2]);
    uint8_t *y = calloc((size_t)W * H, 1), *uv = calloc((size_t)W * H, 1);
    for (int s = 0; s < S; s++) {
        Enc e; EncConfig c = { .width = W, .height = H, .qp_i = 26, .qp_p = 28, .num_refs = 1, .verbose = v };
        if (enc_open(&e, &c)) { printf("session %d: enc_open FAILED (fds %d ports %u)\n", s, open_fds(), ports()); enc_close(&e); return 1; }
        enc_load_nv12(&e, y, W, uv, W);
        EncFrame fr; int rc = enc_encode(&e, false, &fr);
        enc_close(&e);
        if (rc) { printf("session %d: encode failed\n", s); return 1; }
        if (s < 3 || (s + 1) % 50 == 0) printf("session %4d ok: fds %d, mach ports %u, RSS %.1f MB\n", s + 1, open_fds(), ports(), rss_mb());
    }
    printf("RESULT %d sessions ok\n", S);
    return 0;
}
