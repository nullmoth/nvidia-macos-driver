// dsc_extract <shared cache file> <output folder>: unpack the libraries of a dyld shared cache with the system's own
// extractor bundle, so their symbol tables and Objective-C metadata can be read with otool and nm.
#include <dlfcn.h>
#include <stdio.h>
int main(int argc, char **argv) {
    if (argc != 3) { fprintf(stderr, "usage: dsc_extract <cache> <out dir>\n"); return 2; }
    void *h = dlopen("/usr/lib/dsc_extractor.bundle", RTLD_NOW);
    if (!h) { fprintf(stderr, "extractor bundle: %s\n", dlerror()); return 1; }
    int (*extract)(const char *, const char *, void (^)(unsigned, unsigned)) = dlsym(h, "dyld_shared_cache_extract_dylibs_progress");
    if (!extract) { fprintf(stderr, "extractor entry point not found\n"); return 1; }
    int rc = extract(argv[1], argv[2], ^(unsigned cur, unsigned total) { if (cur % 500 == 0) fprintf(stderr, "%u/%u\n", cur, total); });
    fprintf(stderr, "extract rc=%d\n", rc);
    return rc;
}
