// nvencvt-loader: preloaded into Firefox (DYLD_INSERT_LIBRARIES) in place of libnvencvt.dylib. libnvencvt links
// VideoToolbox, CoreMedia and Accelerate, whose start-up threads can hold a lock while Firefox forks its crash helper.
// This loader links only libSystem and opens libnvencvt only in the processes NVENCVT_INJECT_ONLY names (plugin-container),
// early enough for VideoToolbox's first decoder list; libnvencvt registers itself, as DYLD_INSERT_LIBRARIES names an nvencvt.
// Build: xcrun clang -arch x86_64 -mmacosx-version-min=15.0 -O2 -Wall -dynamiclib -o libnvencvt-loader.dylib nvencvt-loader.c
#include <dlfcn.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#define NVENCVT_LIB "/Library/GPUBundles/NVMTLDriver.bundle/Contents/MacOS/libnvencvt.dylib"

static int selected(const char *only, const char *me)
{
    if (!only || !*only || !me) return 0;   // unlike libnvencvt itself, nothing is loaded unless a process is named
    char list[512], *save = NULL;
    snprintf(list, sizeof list, "%s", only);
    for (char *t = strtok_r(list, ", :", &save); t; t = strtok_r(NULL, ", :", &save)) if (!strcmp(t, me)) return 1;
    return 0;
}

__attribute__((constructor)) static void nvencvt_loader(void)
{
    if (!selected(getenv("NVENCVT_INJECT_ONLY"), getprogname())) return;
    if (!dlopen(NVENCVT_LIB, RTLD_NOW | RTLD_LOCAL) && getenv("NVENCVT_LOG")) fprintf(stderr, "nvencvt-loader: %s\n", dlerror());
}
