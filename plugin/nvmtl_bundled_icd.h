/*
 * Local packaging adaptation: keep an ICD and its dispatch hook in the same image.
 * SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0
 */
#ifndef NVMTL_BUNDLED_ICD_H
#define NVMTL_BUNDLED_ICD_H
#include <dlfcn.h>
#include <limits.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <unistd.h>

/* Absolute regular files only. A symlink used as the final component is refused. */
static int nvmtl_icd_regular_path(const char *path, char *out, size_t size)
{
    struct stat st;
    char resolved[PATH_MAX];
    if (!path || path[0] != '/' || !out || !size ||
        strnlen(path, PATH_MAX) == PATH_MAX || lstat(path, &st) ||
        !S_ISREG(st.st_mode) || access(path, R_OK) || !realpath(path, resolved)) return -1;
    size_t n = strlen(resolved);
    if (n >= size) return -1;
    memcpy(out, resolved, n + 1);
    return 0;
}

static int nvmtl_icd_sibling_path(const char *owner, char *out, size_t size)
{
    static const char name[] = "libvulkan_nouveau.dylib";
    char image[PATH_MAX], path[PATH_MAX];
    if (nvmtl_icd_regular_path(owner, image, sizeof image)) return -1;
    const char *slash = strrchr(image, '/');
    if (!slash) return -1;
    size_t prefix = (size_t)(slash - image) + 1;
    if (prefix > sizeof path - sizeof name) return -1;
    memcpy(path, image, prefix);
    memcpy(path + prefix, name, sizeof name);
    return nvmtl_icd_regular_path(path, out, size);
}

/* An override may name this ICD again, but may never introduce a second backend. */
static void *nvmtl_icd_same_image_hook(void *selected, const void *entry,
                                     const char *override, const char *symbol)
{
    Dl_info icd = {0}, hook_image = {0};
    if (!selected || !entry || !symbol || !dladdr(entry, &icd) || !icd.dli_fbase) return NULL;
    if (override && *override) {
        char requested[PATH_MAX], actual[PATH_MAX];
        if (nvmtl_icd_regular_path(override, requested, sizeof requested) ||
            nvmtl_icd_regular_path(icd.dli_fname, actual, sizeof actual) ||
            strcmp(requested, actual)) return NULL;
    }
    void *hook = dlsym(selected, symbol);
    if (!hook || !dladdr(hook, &hook_image) || hook_image.dli_fbase != icd.dli_fbase) return NULL;
    return hook;
}
#endif
