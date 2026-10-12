/* SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0 */
#pragma once
#include <stdint.h>
#include <stddef.h>
#include <string.h>
#if defined(__x86_64__)
#include <immintrin.h>
__attribute__((target("avx2")))
static void nvmtl_upload_stream(uint8_t *dst, const uint8_t *src, size_t tight, size_t pitch, uint32_t rows)
{
    for (uint32_t y = 0; y < rows; y++) {
        size_t x = 0;
        for (; x + 128 <= tight; x += 128) {
            _mm256_stream_si256((__m256i *)(dst + x), _mm256_loadu_si256((const __m256i *)(src + x)));
            _mm256_stream_si256((__m256i *)(dst + x + 32), _mm256_loadu_si256((const __m256i *)(src + x + 32)));
            _mm256_stream_si256((__m256i *)(dst + x + 64), _mm256_loadu_si256((const __m256i *)(src + x + 64)));
            _mm256_stream_si256((__m256i *)(dst + x + 96), _mm256_loadu_si256((const __m256i *)(src + x + 96)));
        }
        for (; x < tight; x += 32)
            _mm256_stream_si256((__m256i *)(dst + x), _mm256_loadu_si256((const __m256i *)(src + x)));
        dst += tight; src += pitch;
    }
    _mm_sfence();
}

#endif

/* GPU consumes this staging memory next. Avoid allocating dirty CPU cache lines
 * for large uploads. Streaming stores require aligned destinations; padded source
 * rows may be unaligned. The fence publishes all stores before Vulkan submission.
 * Small/unaligned transfers and CPUs without AVX2 retain the libc path. */
static inline void nvmtl_upload_copy(void *dst, const void *src, size_t tight, size_t pitch, uint32_t rows)
{
#if defined(__x86_64__)
    if (tight * (size_t)rows >= (1u << 20) && !((uintptr_t)dst & 31) && !(tight & 31) && __builtin_cpu_supports("avx2")) {
        nvmtl_upload_stream(dst, src, tight, pitch, rows);
        return;
    }
#endif
    if (pitch == tight) memcpy(dst, src, tight * (size_t)rows);
    else for (uint32_t y = 0; y < rows; y++)
        memcpy((uint8_t *)dst + (size_t)y * tight, (const uint8_t *)src + (size_t)y * pitch, tight);
}
