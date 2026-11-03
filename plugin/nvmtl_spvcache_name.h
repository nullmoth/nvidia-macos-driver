/* nvmtl_spvcache_name.h - which shader-cache entries the stale sweep keeps. Entry names are
   <digest>.<stage>.<build tag><mode suffixes>.c3.spv, where the build tag names the translator and the suffixes
   (-u2, -p5, -as1, ...) name the modes the process translated in. Pure, so tools/test_app_profile.c proves it. */
#ifndef NVMTL_SPVCACHE_NAME_H
#define NVMTL_SPVCACHE_NAME_H
#include <stdio.h>
#include <string.h>
/* Room for the build tag (sha256-<64 hex>) and every mode suffix, with space to spare. */
#define NVMTL_SPVCACHE_TAG_MAX 160
/* Writes the cache tag for translator build `build` in the given modes. Returns 1, or 0 when the whole tag does not fit
   in `cap` (a cut tag would let two modes share entries); on 0 the caller must not cache. */
static inline int nvmtl_spvcache_mode_tag(char *tag, size_t cap, const char *build, int unroll, int canonical,
                                          int argbuf_samplers, int no_ptr_phi_load, int no_pixel_hw_filter)
{
    if (!tag || !cap || !build) return 0;
    const int k = snprintf(tag, cap, "%s%s%s%s%s%s", build, unroll ? "-u2" : "", canonical ? "-p5" : "",
                           argbuf_samplers ? "-as1" : "", no_ptr_phi_load ? "-np" : "", no_pixel_hw_filter ? "-nf" : "");
    return k > 0 && (size_t)k < cap;
}
/* 1 when `name` is an entry of translator build `build` (any mode); 0 for another build's entry or a foreign file. */
static inline int nvmtl_spvcache_same_build(const char *name, const char *build)
{
    const size_t n = strlen(name), bn = strlen(build);
    if (!bn || n < 7 || strcmp(name + n - 7, ".c3.spv")) return 0;
    for (const char *at = strstr(name, build); at; at = strstr(at + 1, build))
        if (at > name && at[-1] == '.' && (at[bn] == '-' || at[bn] == '.')) return 1;
    return 0;
}
/* The library-index record's file name: idx2-<library sha>.<full cache tag>.plist. Returns 0 when it does not fit. */
static inline int nvmtl_idx_record_name(char *out, size_t cap, const char *libkey, const char *tag)
{
    if (!out || !cap || !libkey || !tag || !*tag) return 0;
    const int k = snprintf(out, cap, "%s.%s.plist", libkey, tag);
    return k > 0 && (size_t)k < cap;
}
#endif
