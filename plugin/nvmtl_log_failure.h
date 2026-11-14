#ifndef NVMTL_LOG_FAILURE_H
#define NVMTL_LOG_FAILURE_H
#include <string.h>
#include <ctype.h>

static inline int nvmtl_log_fault_word(const char *text, const char *word)
{
    const char *p = text;
    while ((p = strstr(p, word))) {
        if ((p == text || !isalnum((unsigned char)p[-1])) &&
            !isalnum((unsigned char)p[5])) return 1;
        p += 5;
    }
    return 0;
}

/* Keep device/feature failures in release syslog. In particular, Vulkan errors
 * such as "vkCreateDevice -> -8" do not contain the word FAIL. */
static inline int nvmtl_log_failure(const char *text)
{
    return text && (strstr(text, "FAIL") || strstr(text, "REFUS") ||
                    nvmtl_log_fault_word(text, "fault") ||
                    nvmtl_log_fault_word(text, "FAULT") || strstr(text, "-> -") ||
                    strstr(text, "missing") || strstr(text, "cannot") ||
                    strstr(text, "unsupported") || strstr(text, "refused") ||
                    /* a Metal 4 commit replayed in parts (indirect threads dispatch): one line per split */
                    strstr(text, "mtl4 split") ||
                    /* a draw the plugin dropped leaves the window blank or garbled with nothing else in the log
                     * (GTX 16 Terminal text on 2.2.0 was framebuffer fetch draws dropped this way) */
                    strstr(text, "draw is skipped") || strstr(text, "draw skipped") ||
                    strstr(text, "draws are being skipped") ||
                    /* a process denied a device: for WindowServer that is the "Failed to create MetalDevice" abort,
                     * and /tmp/nvmtl.log is gone after the reboot that rescues the machine */
                    strstr(text, "no device") ||
                    /* cross-process shared event counters (handles made, imports, wait timeouts): the evidence for
                     * whether a client and WindowServer really share an event; rate-limited by nvlog's per-site dedupe */
                    strstr(text, "xproc-event") ||
                    /* commit-to-completion latency histogram and stuck-commit watchdog (NVMTLEventSync.m) */
                    strstr(text, "commit-latency"));
}
#endif
