// fnmap.c: outline an x86_64 function in this process without a disassembler: direct calls/jumps (E8/E9 rel32), calls
// through the GOT (FF 15 / FF 25 rip-relative) resolved with dladdr, and RIP-relative LEAs that point at CFStrings or C
// strings. Follows internal (non-exported) callees one level when -r is given.
//   ./fnmap SYMBOL [BYTES] [-r]       e.g. ./fnmap VTRegisterVideoEncoderBundleDirectory 400 -r
#include <CoreFoundation/CoreFoundation.h>
#include <dlfcn.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <ctype.h>

extern void *__CFConstantStringClassReference;

static const char *name_of(const void *p, char *buf, size_t n)
{
    Dl_info di;
    const unsigned char *q = p;
    if (q[0] == 0xFF && q[1] == 0x25) {   // a stub: jmp *[rip+disp] -> name the real target
        void *const *slot = (void *const *)(q + 6 + *(const int *)(q + 2));
        if (dladdr(*slot, &di) && di.dli_sname) { snprintf(buf, n, "stub->%s", di.dli_sname); return buf; }
    }
    if (dladdr(p, &di) && di.dli_sname) { snprintf(buf, n, "%s+%ld", di.dli_sname, (long)((const char *)p - (const char *)di.dli_saddr)); return buf; }
    snprintf(buf, n, "%p", p); return buf;
}

static void show_ref(const unsigned char *t)
{
    // a constant CFString: { isa = __CFConstantStringClassReference, flags, char *ptr, long len }
    if (*(void *const *)t == (void *)&__CFConstantStringClassReference) {
        const char *s = *(const char *const *)(t + 16); long len = *(const long *)(t + 24);
        if (len > 0 && len < 200) printf("      cfstr @\"%.*s\"\n", (int)len, s);
        return;
    }
    int ok = 1, n = 0;
    for (; n < 120 && t[n]; n++) if (!isprint(t[n])) { ok = 0; break; }
    if (ok && n >= 3 && !t[n]) printf("      cstr \"%s\"\n", (const char *)t);
}

static void map(const unsigned char *f, int bytes, int depth)
{
    char b[256];
    printf("%*s== %s\n", depth * 2, "", name_of(f, b, sizeof b));
    for (int i = 0; i < bytes; i++) {
        const unsigned char *p = f + i;
        if (p[0] == 0xE8 || p[0] == 0xE9) {
            const unsigned char *t = p + 5 + *(const int *)(p + 1);
            Dl_info di;
            if (dladdr(t, &di) && di.dli_sname && (const unsigned char *)di.dli_saddr == t) {
                printf("%*s  +%03x %s %s\n", depth * 2, "", i, p[0] == 0xE8 ? "call" : "jmp ", di.dli_sname);
                i += 4;
            } else if (dladdr(t, &di) && di.dli_fbase && labs((long)(t - f)) < 0x400000) {
                printf("%*s  +%03x %s %s (internal)\n", depth * 2, "", i, p[0] == 0xE8 ? "call" : "jmp ", name_of(t, b, sizeof b));
                if (depth == 0 && p[0] == 0xE8 && getenv("FNMAP_R")) map(t, 600, 1);
                i += 4;
            }
        } else if (p[0] == 0xFF && (p[1] == 0x15 || p[1] == 0x25)) {
            void *const *slot = (void *const *)(p + 6 + *(const int *)(p + 2));
            Dl_info di;
            if (dladdr(slot, &di) && di.dli_fbase) {
                printf("%*s  +%03x %s *%s\n", depth * 2, "", i, p[1] == 0x15 ? "call" : "jmp ", name_of(*slot, b, sizeof b));
                i += 5;
            }
        } else if ((p[0] == 0x48 || p[0] == 0x4C) && p[1] == 0x8D && (p[2] & 0xC7) == 0x05) {
            const unsigned char *t = p + 7 + *(const int *)(p + 3);
            Dl_info di;
            if (dladdr(t, &di) && di.dli_fbase) { printf("%*s  +%03x lea %s [%p]\n", depth * 2, "", i, name_of(t, b, sizeof b), (void *)t); show_ref(t); i += 6; }
        } else if (p[0] == 0xC3 && i > 16 && !getenv("FNMAP_ALL")) {
            // a ret: stop at the first one past the prologue unless asked for the whole range
        }
    }
}

int main(int argc, char **argv)
{
    if (argc < 2) { fprintf(stderr, "usage: fnmap SYMBOL [BYTES] [-r]\n"); return 2; }
    dlopen("/System/Library/Frameworks/VideoToolbox.framework/VideoToolbox", RTLD_NOW);
    dlopen("/System/Library/Frameworks/CoreMedia.framework/CoreMedia", RTLD_NOW);
    const unsigned char *f = dlsym(RTLD_DEFAULT, argv[1]); if (getenv("FNMAP_OFF")) f += strtol(getenv("FNMAP_OFF"), 0, 0);
    if (getenv("FNMAP_ADDR")) f = (const unsigned char *)strtoull(getenv("FNMAP_ADDR"), 0, 16);
    if (!f) { fprintf(stderr, "%s not found\n", argv[1]); return 1; }
    if (argc > 3 && !strcmp(argv[3], "-r")) setenv("FNMAP_R", "1", 1);
    map(f, argc > 2 ? atoi(argv[2]) : 300, 0);
    return 0;
}
