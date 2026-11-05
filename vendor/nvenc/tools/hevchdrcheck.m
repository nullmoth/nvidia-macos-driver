// hevchdrcheck.m: T6.04. Write our VPS/SPS/PPS (src/hevchdr.c) for a few sizes, let CoreMedia parse them
// (CMVideoFormatDescriptionCreateFromHEVCParameterSets), and save each set as Annex B for scripts/hevcps.py.
//   ./hevchdrcheck [outdir]
#import <Foundation/Foundation.h>
#import <CoreMedia/CoreMedia.h>
#include "hevchdr.h"

int main(int argc, char **argv) { @autoreleasepool {
    const char *dir = argc > 1 ? argv[1] : "/tmp";
    int sizes[][2] = { { 256, 128 }, { 1280, 720 }, { 1920, 1080 }, { 3840, 2160 }, { 3840, 2025 }, { 333, 211 } };
    int bad = 0;
    for (unsigned i = 0; i < sizeof sizes / sizeof sizes[0]; i++) {
        HevcParams p = { .width = sizes[i][0], .height = sizes[i][1], .log2_max_poc_lsb_minus4 = 4, .max_dec_pic_buffering = 5,
                         .sps_rps_negative = 4, .num_ref_idx_l0_default = 1, .pic_init_qp = 26, .vui = 1, .full_range = 1 };
        uint8_t v[256], s[256], q[256]; size_t nv = hevc_write_vps(&p, v), ns = hevc_write_sps(&p, s), nq = hevc_write_pps(&p, q);
        const uint8_t *ps[3] = { v + 4, s + 4, q + 4 }; size_t pl[3] = { nv - 4, ns - 4, nq - 4 };   // without start codes
        CMFormatDescriptionRef fd = NULL;
        OSStatus st = CMVideoFormatDescriptionCreateFromHEVCParameterSets(NULL, 3, ps, pl, 4, NULL, &fd);
        CMVideoDimensions d = fd ? CMVideoFormatDescriptionGetDimensions(fd) : (CMVideoDimensions){ 0, 0 };
        CGRect ca = fd ? CMVideoFormatDescriptionGetCleanAperture(fd, true) : CGRectMake(0, 0, 0, 0);
        // CoreMedia reports the size after the conformance window; 4:2:0 crops in 2-pixel steps, so odd sizes come out one larger
        int ew = (p.width + 1) & ~1, eh = (p.height + 1) & ~1;
        bool ok = !st && d.width == ew && d.height == eh && (int)ca.size.width == ew && (int)ca.size.height == eh;
        printf("%4dx%-4d level %d: VPS %zu SPS %zu PPS %zu bytes -> CoreMedia %d, size %dx%d, clean aperture %.0fx%.0f  %s\n",
               p.width, p.height, hevc_level_for(p.width, p.height), nv, ns, nq, (int)st, d.width, d.height, ca.size.width, ca.size.height, ok ? "OK" : "WRONG");
        if (!ok) bad++;
        if (fd) CFRelease(fd);
        char path[512]; snprintf(path, sizeof path, "%s/ours-%dx%d.h265", dir, p.width, p.height);
        FILE *f = fopen(path, "wb"); fwrite(v, 1, nv, f); fwrite(s, 1, ns, f); fwrite(q, 1, nq, f); fclose(f);
    }
    printf("RESULT %s\n", bad ? "FAIL" : "PASS");
    return bad != 0;
} }
