/* elfhost.h - run an x86-64 Linux shared object inside a macOS x86-64 process. See elfhost.c. */
#pragma once
#include <stddef.h>

typedef struct elfhost_module elfhost_module;

/* Optional caller hooks: bind extra names (e.g. Vulkan entry points) and answer dlopen/dlsym of
 * libraries the object asks for by name. Any hook may be NULL. */
typedef struct {
    void *(*resolve)(void *ctx, const char *name);
    void *(*dlopen_hook)(void *ctx, const char *file, int flags);
    void *(*dlsym_hook)(void *ctx, void *handle, const char *name);
} elfhost_resolver;

elfhost_module *elfhost_load(const char *path, const elfhost_resolver *extra, void *ctx, char *err, size_t errn);
void *elfhost_sym(elfhost_module *m, const char *name);
int elfhost_unresolved(const elfhost_module *m);
