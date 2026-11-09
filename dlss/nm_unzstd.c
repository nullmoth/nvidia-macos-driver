/* nm_unzstd - read a Zstandard stream on stdin, write the decompressed bytes on stdout. Exit 0 only when the whole stream
 * decoded and ended cleanly. Used by dlss_fetch.sh to unpack NVIDIA's driver package, whose payload is zstd-compressed
 * and which macOS cannot decompress itself. Decoder: the Zstandard reference decoder in dlss/zstd (BSD licence). */
#include "zstd/zstd.h"
#include <stdio.h>
#include <stdlib.h>

int main(void)
{
    ZSTD_DCtx *d = ZSTD_createDCtx();
    const size_t inCap = ZSTD_DStreamInSize(), outCap = ZSTD_DStreamOutSize();
    void *ib = malloc(inCap), *ob = malloc(outCap);
    if (!d || !ib || !ob) { fprintf(stderr, "nm_unzstd: out of memory\n"); return 2; }
    size_t last = 1;   /* 0 = a frame just ended */
    size_t n;
    while ((n = fread(ib, 1, inCap, stdin)) > 0) {
        ZSTD_inBuffer in = { ib, n, 0 };
        while (in.pos < in.size) {
            ZSTD_outBuffer out = { ob, outCap, 0 };
            last = ZSTD_decompressStream(d, &out, &in);
            if (ZSTD_isError(last)) { fprintf(stderr, "nm_unzstd: %s\n", ZSTD_getErrorName(last)); return 1; }
            if (out.pos && fwrite(ob, 1, out.pos, stdout) != out.pos) { fprintf(stderr, "nm_unzstd: write failed\n"); return 1; }
        }
    }
    if (ferror(stdin)) { fprintf(stderr, "nm_unzstd: read failed\n"); return 1; }
    if (last != 0) { fprintf(stderr, "nm_unzstd: the stream ends inside a frame\n"); return 1; }
    return fflush(stdout) ? 1 : 0;
}
