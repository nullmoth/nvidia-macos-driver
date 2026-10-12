/* SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0
 * Isolated upload/readback test using a separately built plugin; never installs it.
 * clang -O2 -fobjc-arc tools/bench_surface_upload.m -framework Foundation -framework Metal -o /tmp/bench_surface_upload
 * /tmp/bench_surface_upload /absolute/path/to/NVMTLDriver [source-row-padding]
 * Run baseline/candidate in separate processes against the same installed NVK/kexts.
 */
#import <Foundation/Foundation.h>
#import <Metal/Metal.h>
#include <dlfcn.h>
#include <sys/resource.h>
#include <time.h>
#include "../plugin/nvmtl_vk.h"

static double seconds(void) { return clock_gettime_nsec_np(CLOCK_MONOTONIC_RAW) * 1e-9; }
static double cpu_seconds(void) {
    struct rusage r; getrusage(RUSAGE_SELF, &r);
    return r.ru_utime.tv_sec + r.ru_utime.tv_usec * 1e-6 + r.ru_stime.tv_sec + r.ru_stime.tv_usec * 1e-6;
}

int main(int argc, char **argv) { @autoreleasepool {
    if (argc < 2 || argc > 3) { fprintf(stderr, "usage: %s plugin-binary [row-padding]\n", argv[0]); return 2; }
    char *end = NULL;
    unsigned long padding = argc == 3 ? strtoul(argv[2], &end, 10) : 0;
    if (padding > 4096 || (argc == 3 && (!*argv[2] || *end))) return 2;
    void *library = dlopen(argv[1], RTLD_NOW | RTLD_LOCAL);
    if (!library) { fprintf(stderr, "%s\n", dlerror()); return 3; }
    int (*init)(void) = dlsym(library, "nvmtl_vk_init");
    int (*create)(uint32_t, uint32_t, uint32_t, uint32_t, int, uint32_t, int, uint32_t, uint32_t, nvk_image *) = dlsym(library, "nvmtl_vk_image_create_typed");
    int (*upload)(nvk_queue *, nvk_image *, const void *, size_t) = dlsym(library, "nvmtl_vk_image_write");
    int (*readback)(nvk_queue *, nvk_image *, void *, size_t) = dlsym(library, "nvmtl_vk_image_read");
    void (*destroy)(nvk_image *) = dlsym(library, "nvmtl_vk_image_destroy");
    if (!init || !create || !upload || !readback || !destroy || init()) return 4;
    const uint32_t width = 2880, height = 1800;
    const size_t tight = width * 4, pitch = tight + padding, size = pitch * height;
    uint8_t *input = malloc(size), *output = malloc(size);
    if (!input || !output) return 5;
    for (size_t i = 0; i < size; ++i) input[i] = (uint8_t)(i * 17 + i / 251);
    nvk_image image = {0};
    /* VK_FORMAT_R8G8B8A8_UNORM = 37; this harness intentionally needs no Vulkan headers. */
    if (create(width, height, 37, 4, 0, 1, 0, 2, 1, &image)) return 5;
    for (int pass = 0; pass < 2; ++pass) {
        double wall = 0, cpu = 0;
        for (int iteration = 0; iteration < 40; ++iteration) {
            input[0] = (uint8_t)iteration;
            input[(height - 1) * pitch + tight - 1] = (uint8_t)(iteration + 83);
            memset(output, 0x5a, size);
            double start = seconds(), cpu_start = cpu_seconds();
            if (upload(NULL, &image, input, pitch)) return 6;
            wall += seconds() - start; cpu += cpu_seconds() - cpu_start;
            if (readback(NULL, &image, output, pitch)) return 7;
            for (uint32_t y = 0; y < height; ++y) {
                if (memcmp(input + y * pitch, output + y * pitch, tight)) return 8;
                for (size_t x = tight; x < pitch; ++x) if (output[y * pitch + x] != 0x5a) return 9;
            }
        }
        printf("pass %d pitch %zu upload_ms %.3f upload_cpu_ms %.3f verified\n", pass, pitch, wall * 1000 / 40, cpu * 1000 / 40);
        fflush(stdout);
    }
    destroy(&image); free(input); free(output);
    return 0;
}}
