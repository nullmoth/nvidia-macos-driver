/* nvmtl_failed_air.h - the rules for keeping the AIR of a function the translator refused, so a report can carry it.
   Pure, so tools/test_failed_air.c proves them. The plugin writes the files; the Mac app attaches them to a log report
   only when the user presses Send logs. */
#ifndef NVMTL_FAILED_AIR_H
#define NVMTL_FAILED_AIR_H
#include <stddef.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>

#define NVMTL_FAILED_AIR_MAX_FILES 24u               /* files kept in the folder */
#define NVMTL_FAILED_AIR_MAX_TOTAL (6u << 20)        /* bytes kept in the folder */
#define NVMTL_FAILED_AIR_MAX_FILE  (512u << 10)      /* one function's text; larger ones are not kept */

enum { NVMTL_FAILED_AIR_ADMIT = 0, NVMTL_FAILED_AIR_FULL_FILES = 1, NVMTL_FAILED_AIR_FULL_BYTES = 2, NVMTL_FAILED_AIR_TOO_BIG = 3 };

/* Whether one more file of `size` bytes may be kept when the folder already holds `files` files and `bytes` bytes. */
static inline int nvmtl_failed_air_admit(size_t files, size_t bytes, size_t size)
{
    if (size > NVMTL_FAILED_AIR_MAX_FILE) return NVMTL_FAILED_AIR_TOO_BIG;
    if (files >= NVMTL_FAILED_AIR_MAX_FILES) return NVMTL_FAILED_AIR_FULL_FILES;
    if (bytes + size > NVMTL_FAILED_AIR_MAX_TOTAL) return NVMTL_FAILED_AIR_FULL_BYTES;
    return NVMTL_FAILED_AIR_ADMIT;
}

/* FNV-1a over the module text: the same text is kept once however many processes refuse it. */
static inline uint64_t nvmtl_failed_air_digest(const char *text, size_t n)
{
    uint64_t h = 1469598103934665603ull;
    for (size_t i = 0; i < n; i++) { h ^= (unsigned char)text[i]; h *= 1099511628211ull; }
    return h;
}

/* <function>-<digest>.ll with the function name cut to plain file-name characters (the uploader takes only those).
   Returns 1, or 0 when it does not fit. */
static inline int nvmtl_failed_air_file_name(char *out, size_t cap, const char *function, uint64_t digest)
{
    char clean[64]; size_t k = 0;
    for (const char *p = function ? function : ""; *p && k < sizeof clean - 1; p++) {
        const char c = *p;
        clean[k++] = ((c >= 'A' && c <= 'Z') || (c >= 'a' && c <= 'z') || (c >= '0' && c <= '9') || c == '_') ? c : '_';
    }
    clean[k] = 0;
    if (!k) { clean[0] = 'f'; clean[1] = 0; }
    const int n = snprintf(out, cap, "%s-%016llx.ll", clean, (unsigned long long)digest);
    return n > 0 && (size_t)n < cap;
}

/* The first line of a kept file: why the translator refused it. One line, comment syntax of the text. */
static inline int nvmtl_failed_air_header(char *out, size_t cap, const char *stage, const char *reason)
{
    char r[400]; size_t k = 0;
    for (const char *p = reason ? reason : ""; *p && k < sizeof r - 1; p++) r[k++] = (*p == '\n' || *p == '\r') ? ' ' : *p;
    r[k] = 0;
    const int n = snprintf(out, cap, "; translator refusal (%s): %s\n", stage ? stage : "unknown", r);
    return n > 0 && (size_t)n < cap;
}
#endif
