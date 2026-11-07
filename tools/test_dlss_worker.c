/*
 * NullMoth NVIDIA driver for macOS
 * Copyright (c) 2026 NullMoth Systems.
 * SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0
 */

/* Local check for the DLSS worker thread: builds nvmtl_dlss.c with stand-ins for the ELF host and the log, then runs
 * its selftest. Build and run:
 *   clang -I plugin -I <vulkan headers> tools/test_dlss_worker.c -o /tmp/test_dlss_worker && /tmp/test_dlss_worker
 */
#include <stdarg.h>
#include <stdio.h>
#include "../plugin/dlss/elfhost.h"
void nvlog(const char *fmt, ...) { va_list a; va_start(a, fmt); vfprintf(stderr, fmt, a); fputc('\n', stderr); va_end(a); }
elfhost_module *elfhost_load(const char *p, const elfhost_resolver *x, void *c, char *e, size_t n) { return 0; }
void *elfhost_sym(elfhost_module *m, const char *n) { return 0; }
#include "../plugin/nvmtl_dlss.c"
int main(void) { return nvmtl_dlss_selftest(); }
