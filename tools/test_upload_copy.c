/* SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0
 * clang -O2 -fsanitize=address,undefined tools/test_upload_copy.c -o /tmp/test_upload_copy
 */
#include "../plugin/nvmtl_upload_copy.h"
#include <stdio.h>
#include <stdlib.h>

static int check(size_t tight, size_t padding, uint32_t rows, size_t offset)
{
    size_t pitch = tight + padding, bytes = tight * rows;
    uint8_t *source, *allocation;
    if (posix_memalign((void **)&source, 64, pitch * rows + 64) ||
        posix_memalign((void **)&allocation, 64, bytes + 128)) abort();
    uint8_t *dst = allocation + 64 + offset;
    memset(allocation, 0x5a, bytes + 128);
    for (size_t i = 0; i < pitch * rows + 64; ++i) source[i] = (uint8_t)(i * 17 + i / 251);
    nvmtl_upload_copy(dst, source + 1, tight, pitch, rows);
    int bad = 0;
    for (uint32_t y = 0; y < rows; ++y)
        bad |= memcmp(dst + (size_t)y * tight, source + 1 + (size_t)y * pitch, tight) != 0;
    for (size_t i = 0; i < 64 + offset; ++i) bad |= allocation[i] != 0x5a;
    for (size_t i = 64 + offset + bytes; i < bytes + 128; ++i) bad |= allocation[i] != 0x5a;
    free(source); free(allocation);
    if (bad) fprintf(stderr, "FAIL tight=%zu padding=%zu rows=%u offset=%zu\n", tight, padding, rows, offset);
    return bad;
}

int main(void)
{
    int failures = 0;
    const size_t widths[] = {1, 31, 32, 96, 128, 4096, 4097, 11520};
    for (size_t i = 0; i < sizeof widths / sizeof widths[0]; ++i)
        for (size_t padding = 0; padding <= 65; padding += 65)
            for (size_t offset = 0; offset <= 1; ++offset) {
                failures += check(widths[i], padding, 3, offset);
                failures += check(widths[i], padding, 32768 / widths[i] + 1024, offset);
            }
    failures += check(32, 1, 32768, 0);
    failures += check(11520, 64, 1800, 0);
    printf("upload-copy correctness: %s\n", failures ? "FAIL" : "PASS");
    return failures != 0;
}
