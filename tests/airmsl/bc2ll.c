// bc2ll <in.air> -o <out.ll>: bitcode to text the way the plugin does it, through the bundled libLLVM's C API
// (LLVMParseBitcodeInContext2 + LLVMPrintModuleToString). NVMTL_LIBLLVM names the libLLVM.dylib (the one from
// NVMTLDriver.bundle/Contents/MacOS/air-runtime); it must match this tool's architecture.
#include <dlfcn.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
int main(int argc, char **argv) {
    if (argc != 4 || strcmp(argv[2], "-o")) { fprintf(stderr, "usage: bc2ll in.air -o out.ll\n"); return 3; }
    const char *lib = getenv("NVMTL_LIBLLVM");
    void *h = lib ? dlopen(lib, RTLD_NOW | RTLD_LOCAL) : NULL;
    if (!h) { fprintf(stderr, "dlopen %s: %s\n", lib ? lib : "(NVMTL_LIBLLVM unset)", dlerror()); return 3; }
    void *(*ctx_create)(void) = dlsym(h, "LLVMContextCreate");
    void *(*mem_buf)(const char *, size_t, const char *) = dlsym(h, "LLVMCreateMemoryBufferWithMemoryRangeCopy");
    int (*parse)(void *, void *, void **) = dlsym(h, "LLVMParseBitcodeInContext2");
    char *(*print)(void *) = dlsym(h, "LLVMPrintModuleToString");
    if (!ctx_create || !mem_buf || !parse || !print) { fprintf(stderr, "missing LLVM-C symbol\n"); return 3; }
    FILE *f = fopen(argv[1], "rb");
    if (!f) { perror(argv[1]); return 3; }
    fseek(f, 0, SEEK_END); long n = ftell(f); fseek(f, 0, SEEK_SET);
    char *data = malloc(n);
    if (fread(data, 1, n, f) != (size_t)n) return 3;
    fclose(f);
    void *ctx = ctx_create(), *buf = mem_buf(data, n, "nvmtl-air"), *mod = NULL;
    if (parse(ctx, buf, &mod) || !mod) { fprintf(stderr, "bitcode parse failed\n"); return 1; }
    char *text = print(mod);
    FILE *o = fopen(argv[3], "wb"); fputs(text, o); fclose(o);
    return 0;
}
