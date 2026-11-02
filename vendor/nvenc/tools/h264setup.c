// h264setup.c: print the fields that differ between two H.264 picture setup buffers (nvenc_h264_drv_pic_setup_s + the
// controls it points to), from binary dumps (scripts/trace2blob.py) or our encoder's. T7.02.
//   ./h264setup a.bin b.bin
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include "nvenc_drv.h"
#include "nvencdump264.h"

static unsigned char A[32768], B[32768];

int main(int argc, char **argv)
{
    if (argc < 3) { fprintf(stderr, "usage: h264setup a.bin b.bin\n"); return 2; }
    FILE *f = fopen(argv[1], "rb"); if (!f || !fread(A, 1, sizeof A, f)) { perror(argv[1]); return 1; } fclose(f);
    f = fopen(argv[2], "rb"); if (!f || !fread(B, 1, sizeof B, f)) { perror(argv[2]); return 1; } fclose(f);
    nvenc_h264_drv_pic_setup_s *a = (void *)A, *b = (void *)B;
    printf("picture setup:\n"); dump_nvenc_h264_drv_pic_setup_s(a, b, "");
    nvenc_h264_pic_control_s *c = &a->pic_control, *d = &b->pic_control;
    printf("me_control @0x%x:\n", c->me_control_offset); dump_nvenc_h264_me_control_s((void *)(A + c->me_control_offset), (void *)(B + d->me_control_offset), "");
    printf("md_control @0x%x:\n", c->md_control_offset); dump_nvenc_h264_md_control_s((void *)(A + c->md_control_offset), (void *)(B + d->md_control_offset), "");
    printf("q_control @0x%x:\n", c->q_control_offset); dump_nvenc_h264_quant_control_s((void *)(A + c->q_control_offset), (void *)(B + d->q_control_offset), "");
    printf("slice_control @0x%x:\n", c->slice_control_offset); dump_nvenc_h264_slice_control_s((void *)(A + c->slice_control_offset), (void *)(B + d->slice_control_offset), "");
    return 0;
}
