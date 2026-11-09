/*
 * NullMoth NVIDIA driver for macOS
 * Copyright (c) 2026 NullMoth Systems.
 * SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0
 */

/*
 * Which processes get the NVIDIA GPU as a Metal device on a hybrid laptop. Plain C so tools/test_gpu_policy.c runs the same
 * code on the host; NVMTLDevice.m supplies the inputs.
 *
 * Only active when the hybrid switch (boot-arg nmhybrid) is on, and only matters when another GPU also offers Metal
 * (an integrated GPU macOS still drives, such as Coffee Lake). With no second Metal GPU the NVIDIA device is the only
 * choice and is always given.
 *
 * Policy file /Library/GPUBundles/nvmtl-gpu-policy.txt, one rule per line, '#' starts a comment:
 *     mode nvidia          (default) every process gets the NVIDIA device
 *     mode apple           follow Apple's rule: a process whose Info.plist sets NSSupportsAutomaticGraphicsSwitching to
 *                          true is content with the low-power GPU and does not get the NVIDIA device; every other process
 *                          keeps the high-performance GPU
 *     igpu <process>       this process never gets the NVIDIA device
 *     nvidia <process>     this process always gets it
 * A per-process rule beats the mode. The plugin can give a process the device or not; it cannot list a device without
 * making it the default, so a process the policy sends to the low-power GPU does not see the NVIDIA device at all.
 */
#ifndef NVMTL_GPU_POLICY_H
#define NVMTL_GPU_POLICY_H

#include <string.h>

enum { NMP_PREF_AUTO = 0, NMP_PREF_NVIDIA = 1, NMP_PREF_IGPU = 2 };

/* Reads the policy text. Returns the per-process preference; *mode_apple is 1 for "mode apple". */
static inline int nmp_parse(const char *text, const char *proc, int *mode_apple)
{
    int pref = NMP_PREF_AUTO;
    *mode_apple = 0;
    if (!text) return pref;
    const char *p = text;
    while (*p) {
        char line[200]; size_t n = 0;
        while (*p && *p != '\n' && *p != '\r') { if (n + 1 < sizeof line) line[n++] = *p; p++; }
        while (*p == '\n' || *p == '\r') p++;
        line[n] = 0;
        char *q = line; while (*q == ' ' || *q == '\t') q++;
        if (!*q || *q == '#') continue;
        if (!strncmp(q, "mode ", 5)) {
            char *v = q + 5; while (*v == ' ' || *v == '\t') v++;
            if (!strcmp(v, "apple")) *mode_apple = 1; else if (!strcmp(v, "nvidia")) *mode_apple = 0;
        } else if (!strncmp(q, "igpu ", 5) || !strncmp(q, "nvidia ", 7)) {
            int ig = q[0] == 'i';
            char *v = q + (ig ? 5 : 7); while (*v == ' ' || *v == '\t') v++;
            size_t l = strlen(v); while (l && (v[l - 1] == ' ' || v[l - 1] == '\t')) v[--l] = 0;
            if (proc && !strcmp(v, proc)) pref = ig ? NMP_PREF_IGPU : NMP_PREF_NVIDIA;   /* the last matching rule wins */
        }
    }
    return pref;
}

/* 1 = give the process the NVIDIA device. app_auto_switch: the Info.plist value, -1 when absent. */
static inline int nmp_decide(int hybrid_level, int mode_apple, int pref, int app_auto_switch, int other_metal_gpu, const char **why)
{
    const char *w = "";
    int r = 1;
    if (hybrid_level <= 0)              { w = "hybrid switch off"; r = 1; }
    else if (!other_metal_gpu)          { w = "no other Metal GPU to use instead"; r = 1; }
    else if (pref == NMP_PREF_IGPU)     { w = "policy: this process uses the integrated GPU"; r = 0; }
    else if (pref == NMP_PREF_NVIDIA)   { w = "policy: this process uses the NVIDIA GPU"; r = 1; }
    else if (!mode_apple)               { w = "mode nvidia"; r = 1; }
    else if (app_auto_switch == 1)      { w = "mode apple: the app allows automatic graphics switching"; r = 0; }
    else                                { w = "mode apple: the app does not opt into automatic switching"; r = 1; }
    if (why) *why = w;
    return r;
}

#endif
