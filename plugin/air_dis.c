/* nvmtl-air-dis -S <in.air> -o <out.ll>: print a Metal AIR bitcode module as LLVM assembly.
 *
 * The plugin decodes AIR in process with the LLVM library it ships (air-runtime/libLLVM.dylib, upstream LLVM). When that
 * is not possible (a library over the in-process size cap, or a reader the plugin could not load), it runs this tool, so
 * a malformed module costs a failed translation instead of the app. It uses the same upstream library, found next to
 * this executable, and therefore prints exactly what the in-process path prints. Arguments are the ones the plugin
 * passes; anything else is refused. Exit 0 on success, 1 on a parse or write failure, 2 on bad arguments or setup. */
#include <dlfcn.h>
#include <limits.h>
#include <mach-o/dyld.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>

/* Out of process the size cap is only a memory bound, far above any shader library a real app ships. */
#define AIR_DIS_MAX_BYTES (1024L * 1024L * 1024L)

static int g_error;
static int (*p_severity)(void *);
static void on_diagnostic(void *d, void *ctx) { (void)ctx; if (p_severity && p_severity(d) == 0) g_error = 1; }

static int library_path(char *out, size_t cap) {
    char exe[PATH_MAX], real[PATH_MAX];
    uint32_t n = sizeof exe;
    if (_NSGetExecutablePath(exe, &n) != 0 || !realpath(exe, real)) return -1;
    char *slash = strrchr(real, '/');
    if (!slash) return -1;
    *slash = 0;
    return snprintf(out, cap, "%s/air-runtime/libLLVM.dylib", real) < (int)cap ? 0 : -1;
}

int main(int argc, char **argv) {
    if (argc != 5 || strcmp(argv[1], "-S") || strcmp(argv[3], "-o")) {
        fprintf(stderr, "usage: nvmtl-air-dis -S <in.air> -o <out.ll>\n");
        return 2;
    }
    const char *in = argv[2], *out = argv[4];
    char lib[PATH_MAX];
    if (library_path(lib, sizeof lib)) { fprintf(stderr, "cannot locate air-runtime\n"); return 2; }
    void *h = dlopen(lib, RTLD_NOW | RTLD_LOCAL);
    if (!h) { fprintf(stderr, "%s\n", dlerror()); return 2; }
    void *(*ctx_create)(void) = dlsym(h, "LLVMContextCreate");
    void (*ctx_dispose)(void *) = dlsym(h, "LLVMContextDispose");
    void (*set_handler)(void *, void (*)(void *, void *), void *) = dlsym(h, "LLVMContextSetDiagnosticHandler");
    void *(*buf_copy)(const char *, size_t, const char *, int) = dlsym(h, "LLVMCreateMemoryBufferWithMemoryRangeCopy");
    void (*buf_dispose)(void *) = dlsym(h, "LLVMDisposeMemoryBuffer");
    int (*parse)(void *, void *, void **) = dlsym(h, "LLVMParseBitcodeInContext2");
    int (*print)(void *, const char *, char **) = dlsym(h, "LLVMPrintModuleToFile");
    void (*mod_dispose)(void *) = dlsym(h, "LLVMDisposeModule");
    void (*msg_dispose)(char *) = dlsym(h, "LLVMDisposeMessage");
    p_severity = dlsym(h, "LLVMGetDiagInfoSeverity");
    if (!ctx_create || !ctx_dispose || !set_handler || !buf_copy || !buf_dispose || !parse || !print || !mod_dispose || !msg_dispose) {
        fprintf(stderr, "air-runtime lacks a required LLVM entry point\n");
        return 2;
    }
    FILE *f = fopen(in, "rb");
    struct stat st;
    if (!f || fstat(fileno(f), &st) || !S_ISREG(st.st_mode) || st.st_size < 4 || st.st_size > AIR_DIS_MAX_BYTES) {
        fprintf(stderr, "cannot read %s as a bitcode file\n", in);
        if (f) fclose(f);
        return 1;
    }
    char *bytes = malloc((size_t)st.st_size);
    size_t got = bytes ? fread(bytes, 1, (size_t)st.st_size, f) : 0;
    fclose(f);
    if (got != (size_t)st.st_size) { fprintf(stderr, "short read on %s\n", in); free(bytes); return 1; }

    void *ctx = ctx_create();
    set_handler(ctx, on_diagnostic, NULL);
    void *buf = buf_copy(bytes, got, "nvmtl-air", 0), *module = NULL;
    free(bytes);
    int rc = 1;
    g_error = 0;
    if (buf && !parse(ctx, buf, &module) && module && !g_error) {
        char *msg = NULL;
        if (print(module, out, &msg) == 0) rc = 0;
        else fprintf(stderr, "cannot write %s: %s\n", out, msg ? msg : "?");
        if (msg) msg_dispose(msg);
    } else {
        fprintf(stderr, "%s is not a bitcode module this LLVM can read\n", in);
    }
    if (module) mod_dispose(module);
    if (buf) buf_dispose(buf);   /* LLVMParseBitcodeInContext2 copies what it needs; the buffer stays ours */
    ctx_dispose(ctx);
    if (rc) remove(out);
    return rc;
}
