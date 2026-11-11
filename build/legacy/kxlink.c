// kxlink.c: run the kernel's own kext linker (libkxld, built from the release's published kernel source) over one
// kext, outside any kernel. It answers one question per kext and release: does the linker that will load this
// kext accept it, with its class tables patched against the release's own families.
//
//   kxlink [-v] [-o linked.out] <kext binary> <bundle id> { -d <binary>[,<interface binary>] | -i <same> } ...
//     -d  a direct dependency, -i an indirect one (needed only so parent class tables can be found)
//     for a kernel programming interface, <binary> is the kernel and <interface binary> the interface kext
// All inputs are thin x86_64 files. Exit 0 = linked, 1 = the linker refused (its messages are printed), 2 = usage.
#include <fcntl.h>
#include <mach/machine.h>
#include <stdarg.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <unistd.h>
#include <mach-o/loader.h>
#include <mach-o/nlist.h>
#include <stdint.h>
#include <libkern/kxld.h>

static int verbose, nerr, nwarn;
// Each kext linked in one test gets its own range (-a); the linker looks parent methods up by address across all
// dependencies, so two images at one address would answer for each other.
static kxld_addr_t load_addr = 0xffffff7f90000000ULL;

static u_char *slurp(const char *path, u_long *size) {
    struct stat st; int fd = open(path, O_RDONLY);
    if (fd < 0 || fstat(fd, &st) != 0) { fprintf(stderr, "kxlink: cannot read %s\n", path); exit(2); }
    u_char *p = malloc((size_t)st.st_size + 1);
    if (!p || read(fd, p, (size_t)st.st_size) != st.st_size) { fprintf(stderr, "kxlink: short read %s\n", path); exit(2); }
    close(fd); *size = (u_long)st.st_size; return p;
}

// Size of a 64-bit Mach-O image in memory as a file: the furthest byte any load command points at.
static size_t macho_size(const u_char *p) {
    const struct mach_header_64 *h = (const void *)p; size_t end = sizeof *h + h->sizeofcmds;
    const struct load_command *lc = (const void *)(h + 1);
    for (uint32_t i = 0; i < h->ncmds; i++, lc = (const void *)((const u_char *)lc + lc->cmdsize)) {
        size_t e = 0;
        if (lc->cmd == LC_SEGMENT_64) { const struct segment_command_64 *s = (const void *)lc; e = (size_t)(s->fileoff + s->filesize); }
        else if (lc->cmd == LC_SYMTAB) {
            const struct symtab_command *s = (const void *)lc; e = s->stroff + s->strsize;
            size_t e2 = s->symoff + (size_t)s->nsyms * sizeof(struct nlist_64); if (e2 > e) e = e2;
        } else if (lc->cmd == LC_DYSYMTAB) {
            const struct dysymtab_command *s = (const void *)lc; e = s->locreloff + (size_t)s->nlocrel * 8;
            size_t e2 = s->extreloff + (size_t)s->nextrel * 8; if (e2 > e) e = e2;
        }
        if (e > end) end = e;
    }
    return end;
}

// The linker asks where the kext will live. A fixed address in the kext region is enough for a link test; with
// default flags the linker keeps its own output buffer and hands it back.
static kxld_addr_t alloc_cb(size_t size, KXLDAllocateFlags *flags, void *user) {
    (void)size; (void)user; *flags = kKxldAllocateDefault; return load_addr;
}

static void log_cb(KXLDLogSubsystem sys, KXLDLogLevel level, const char *fmt, va_list ap, void *user) {
    (void)sys;
    if (level == kKxldLogErr) nerr++; else if (level == kKxldLogWarn) nwarn++;
    if (level > kKxldLogWarn && level != kKxldLogExplicit && !verbose) return;
    static const char *names[] = { "note", "ERROR", "warning", "basic", "detail", "debug" };
    printf("  [%s] %s: ", names[level <= kKxldLogDebug ? level : 5], user ? (const char *)user : "?");
    vprintf(fmt, ap); printf("\n");
}

int main(int argc, char **argv) {
    u_int ndeps = 0; const char *out = NULL, *target = NULL, *name = NULL, *state_out = NULL;
#if KX_API == 1     // 10.6: a dependency is the link state an earlier link wrote, not a binary
    u_char *deps[64];
#else
    KXLDDependency deps[64];
#endif
    memset(deps, 0, sizeof deps);
    for (int i = 1; i < argc; i++) {
        if (!strcmp(argv[i], "-v")) verbose = 1;
        else if (!strcmp(argv[i], "-o") && i + 1 < argc) out = argv[++i];
        else if (!strcmp(argv[i], "-a") && i + 1 < argc) load_addr = strtoull(argv[++i], NULL, 16);
        else if (!strcmp(argv[i], "-s") && i + 1 < argc) state_out = argv[++i];
#if KX_API == 1
        else if ((!strcmp(argv[i], "-d") || !strcmp(argv[i], "-i")) && i + 1 < argc && ndeps < 64) { u_long sz; deps[ndeps++] = slurp(argv[++i], &sz); }
#else
        else if ((!strcmp(argv[i], "-d") || !strcmp(argv[i], "-i")) && i + 1 < argc && ndeps < 64) {
            int direct = argv[i][1] == 'd'; char *spec = strdup(argv[++i]); char *comma = strchr(spec, ',');
            if (comma) *comma = 0;
            deps[ndeps].kext = slurp(spec, &deps[ndeps].kext_size); deps[ndeps].kext_name = spec;
            if (comma) { deps[ndeps].interface = slurp(comma + 1, &deps[ndeps].interface_size); deps[ndeps].interface_name = comma + 1; }
            deps[ndeps].is_direct_dependency = direct; ndeps++;
        }
#endif
        else if (!target) target = argv[i];
        else if (!name) name = argv[i];
        else { fprintf(stderr, "kxlink: unexpected argument %s\n", argv[i]); return 2; }
    }
#if KX_API == 1
    if (!target || !name) {
#else
    if (!target || !name || !ndeps) {
#endif
        fprintf(stderr, "usage: kxlink [-v] [-o out] <kext binary> <bundle id> -d dep[,interface] ...\n"); return 2; }
    u_long size; u_char *file = slurp(target, &size);
    KXLDContext *ctx = NULL;
#if KX_API == 1
    kern_return_t rc = kxld_create_context(&ctx, alloc_cb, log_cb, kKxldFlagDefault, CPU_TYPE_X86_64, CPU_SUBTYPE_X86_64_ALL);
#elif KX_API >= 3
    kern_return_t rc = kxld_create_context(&ctx, alloc_cb, log_cb, kKxldFlagDefault, CPU_TYPE_X86_64, CPU_SUBTYPE_X86_64_ALL, 4096);
#else   // before 10.11 the linker took no page size
    kern_return_t rc = kxld_create_context(&ctx, alloc_cb, log_cb, kKxldFlagDefault, CPU_TYPE_X86_64, CPU_SUBTYPE_X86_64_ALL);
#endif
    if (rc != KERN_SUCCESS) { printf("kxlink: no linker context (%d)\n", rc); return 2; }
    u_char *linked = NULL; kxld_addr_t kmod_info = 0;
#if KX_API == 1
    u_char *state = NULL; u_long state_size = 0;
    rc = kxld_link_file(ctx, file, size, name, (void *)name, ndeps ? deps : NULL, ndeps, &linked, &kmod_info, &state, &state_size, NULL, NULL);
    if (rc == KERN_SUCCESS && state_out && state) { FILE *f = fopen(state_out, "wb"); if (f) { fwrite(state, 1, state_size, f); fclose(f); } }
#else
    (void)state_out;
    rc = kxld_link_file(ctx, file, size, name, (void *)name, deps, ndeps, &linked, &kmod_info);
#endif
    printf("%s: %s (linker return %d, %d errors, %d warnings, module info at 0x%llx, %u dependencies)\n",
           name, rc == KERN_SUCCESS ? "LINKED" : "REFUSED", rc, nerr, nwarn, (unsigned long long)kmod_info, ndeps);
    if (rc == KERN_SUCCESS && out && linked) {
        // the linked image is a Mach-O; its size is not returned, so write what its load commands say via the caller's tools
        FILE *f = fopen(out, "wb"); if (f) { fwrite(linked, 1, macho_size(linked), f); fclose(f); }
    }
    kxld_destroy_context(ctx);
    return rc == KERN_SUCCESS ? 0 : 1;
}
