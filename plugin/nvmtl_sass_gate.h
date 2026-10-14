#ifndef NVMTL_SASS_GATE_H
#define NVMTL_SASS_GATE_H
/*
 * Which GPUs may run the shipped compiled kernels. The machine code is sm_120, which among the cards
 * this driver supports means the GeForce RTX 50 series only, read from the name NVK reports
 * ("NVIDIA GeForce RTX 5070 Laptop GPU"). The model number must be exactly four digits starting with
 * 50: "RTX 5000 Ada Generation" is an Ada workstation card and must not match. NVK checks the
 * architecture again before it records a dispatch.
 */
#include <string.h>

static inline int nvmtl_sass_device_name_ok(const char *name)
{
    const char *p = name ? strstr(name, "GeForce RTX 50") : NULL;
    if (!p) return 0;
    p += strlen("GeForce RTX ");
    for (int i = 0; i < 4; i++) if (p[i] < '0' || p[i] > '9') return 0;
    return p[4] == 0 || p[4] == ' ';
}

#endif
