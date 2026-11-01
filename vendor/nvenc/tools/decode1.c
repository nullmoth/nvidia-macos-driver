// decode1.c (M8, T8.03): decode the first picture (an IDR) of an Annex B H.264 file on NVDEC and compare it with a
// reference decode. Everything comes from open-gpu-doc's nvdec_drv.h / clc9b0.h and the H.264 standard (user,
// 2026-10-19: headers only); buffer sizes the headers do not give are generous guesses, reported on the way.
//   ./decode1 in.h264 ref.yuv [tileFormat [gobHeight]]     ref.yuv: I420 (ffmpeg -f rawvideo -pix_fmt yuv420p)
//   tileFormat 0 TBL, 1 KBL (default), 2 Tile16x16; gobHeight 0..4 = GOB_2..GOB_32 (default 0)
// Writes /tmp/decode1-luma.raw and -chroma.raw (the surfaces as NVDEC wrote them) for layout analysis.
#include "rm.h"
#include "channel.h"
#include "blocklinear.h"
#include "h264parse.h"
#include <math.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>
#include "class/clc56f.h"
#include "class/cl2080_notification.h"
#include "clc9b0.h"
#include "nvdec_drv.h"

static uint8_t *slurp(const char *p, size_t *n)
{
    FILE *f = fopen(p, "rb"); if (!f) { perror(p); exit(1); }
    fseek(f, 0, SEEK_END); *n = (size_t)ftell(f); fseek(f, 0, SEEK_SET);
    uint8_t *b = malloc(*n); if (fread(b, 1, *n, f) != *n) exit(1); fclose(f); return b;
}
static double psnr(const uint8_t *a, const uint8_t *b, size_t n)
{
    double se = 0; for (size_t i = 0; i < n; i++) { int d = a[i] - b[i]; se += d * d; }
    return se == 0 ? 99.0 : 10 * log10(255.0 * 255.0 / (se / n));
}
static NvU32 hi(NvU64 a) { return (NvU32)(a >> 8); }

int main(int argc, char **argv)
{
    if (argc < 3) { fprintf(stderr, "usage: decode1 in.h264 ref.yuv [tileFormat [gobHeight]]\n"); return 2; }
    const unsigned tileFormat = argc > 3 ? (unsigned)atoi(argv[3]) : 1, gob = argc > 4 ? (unsigned)atoi(argv[4]) : 0;
    size_t n, rn; uint8_t *in = slurp(argv[1], &n), *ref = slurp(argv[2], &rn);

    // ---- parse up to the end of the first picture ----
    static H264Params p; H264Slice first = { 0 }; int nslices = 0;
    static uint8_t bs[16 << 20]; size_t bsn = 0; NvU32 sliceOff[256];
    size_t pos = 0; const uint8_t *nal; size_t len;
    while (h264_next_annexb(in, n, &pos, &nal, &len)) {
        int t = nal[0] & 31;
        if (t == H264_NAL_SPS) h264_parse_sps(&p, nal, len);
        else if (t == H264_NAL_PPS) h264_parse_pps(&p, nal, len);
        else if (t == H264_NAL_SLICE || t == H264_NAL_IDR) {
            H264Slice s; if (h264_parse_slice_header(&p, nal, len, &s)) { fprintf(stderr, "slice header parse failed\n"); return 1; }
            if (nslices && s.first_mb_in_slice == 0) break;             // the next picture starts
            if (!nslices) first = s;
            if (nslices >= 256 || bsn + len + 3 > sizeof bs) break;
            sliceOff[nslices++] = (NvU32)bsn;
            bs[bsn++] = 0; bs[bsn++] = 0; bs[bsn++] = 1; memcpy(bs + bsn, nal, len); bsn += len;
        }
    }
    if (!nslices || !first.idr) { fprintf(stderr, "no IDR picture at the start\n"); return 1; }
    const H264Pps *pps = &p.pps[first.pps_id]; const H264Sps *sps = &p.sps[pps->sps_id];
    const unsigned wmb = (unsigned)sps->pic_width_in_mbs_minus1 + 1, hmb = (unsigned)(sps->pic_height_in_map_units_minus1 + 1) * (2u - (unsigned)sps->frame_mbs_only_flag);
    const unsigned W = wmb * 16, H = hmb * 16;
    printf("picture: %ux%u (%u x %u MBs, cropped %dx%d), %d slice(s), %zu bytes, profile %d cabac %d t8x8 %d\n", W, H, wmb, hmb,
           sps->width, sps->height, nslices, bsn, sps->profile_idc, pps->entropy_coding_mode_flag, pps->transform_8x8_mode_flag);

    // ---- RM, channel ----
    Rm rm = { 0 }; if (rm_open(&rm) || rm_va_init(&rm)) { fprintf(stderr, "RM failed\n"); return 1; }
    Chan c; NV_NVDEC_ALLOCATION_PARAMETERS dp = { .size = sizeof dp, .prohibitMultipleInstances = 0, .engineInstance = 0 };
    if (chan_create(&rm, &c, NV2080_ENGINE_TYPE_NVDEC0, rm_class(&rm, RM_CLS_NVDEC), &dp)) { fprintf(stderr, "channel failed\n"); return 1; }

    // ---- buffers (system memory, CPU- and GPU-mapped, so everything can be inspected) ----
    const size_t pitch = (W + 63) & ~(size_t)63, bh = gob + 1;            // GOB_2 = blocks of 2 GOBs = block_height_log2 1
    const size_t lumaSize = bl_plane_size(pitch, H, (unsigned)bh) + 65536, chromaSize = bl_plane_size(pitch, H / 2, (unsigned)bh) + 65536;
    const size_t colocSize = (size_t)wmb * hmb * 256 * 2, histSize = (size_t)(getenv("DECODE1_HIST_MB") ? atoi(getenv("DECODE1_HIST_MB")) : 8) << 20;
    RmMem setup, bsb, offs, status, coloc, hist, luma, chroma, sem, scratch, histo, mbhist, dispL, dispC;
    struct { RmMem *m; size_t size; const char *what; } bufs[] = {
        { &setup, 0x10000, "picture setup" }, { &bsb, (bsn + 0x10000) & ~(size_t)0xffff, "bitstream" }, { &offs, 0x1000, "slice offsets" },
        { &status, 0x1000, "status" }, { &coloc, colocSize, "co-located MVs (guess 256 B/MB)" }, { &hist, histSize, "history (guess 8 MB)" },
        { &luma, lumaSize, "luma surface" }, { &chroma, chromaSize, "chroma surface" }, { &sem, 0x1000, "semaphore" },
        { &scratch, 32u << 20, "picture scratch (guess 32 MB)" }, { &histo, 1u << 20, "histogram (guess 1 MB)" },
        { &mbhist, 8u << 20, "H.264 MB history (guess 8 MB)" },
        { &dispL, (size_t)W * H + 0x100000, "display luma" }, { &dispC, (size_t)W * H / 2 + 0x100000, "display chroma" } };
    // DECODE1_PAD=<bit mask of buffers, in the order above>:<MB>: give those buffers that much extra (MMU fault hunt)
    unsigned padMask = 0, padMB = 0; if (getenv("DECODE1_PAD")) sscanf(getenv("DECODE1_PAD"), "%x:%u", &padMask, &padMB);
    for (size_t i = 0; i < sizeof bufs / sizeof bufs[0]; i++) if (padMask >> i & 1) bufs[i].size += (size_t)padMB << 20;
    for (size_t i = 0; i < sizeof bufs / sizeof bufs[0]; i++) {
        if (rm_mem_alloc(&rm, bufs[i].m, (bufs[i].size + 0xfff) & ~(size_t)0xfff, 0x1000, true, true, true)) { fprintf(stderr, "alloc %s failed\n", bufs[i].what); return 1; }
        memset(bufs[i].m->cpu, 0, bufs[i].m->size);
    }
    // DECODE1_EOS: 1 = append an end-of-stream NAL (00 00 01 0b) to the bitstream, 2 = pass it in eos[] with
    // explicitEOSPresentFlag, 3 = both. Without it the last rows of the picture were not written (T8.03).
    const int eosMode = getenv("DECODE1_EOS") ? atoi(getenv("DECODE1_EOS")) : 2;   // 2 decodes bit-exact (T8.03)
    static const uint8_t eosNal[16] = { 0, 0, 1, 0x0b, 0, 0, 1, 0x0b };
    if (eosMode & 1) { memcpy(bs + bsn, eosNal, 4); bsn += 4; }
    memcpy(bsb.cpu, bs, bsn);
    memcpy(offs.cpu, sliceOff, sizeof(NvU32) * (size_t)nslices);

    // ---- picture setup (nvdec_h264_pic_s) ----
    nvdec_h264_pic_s *ps = setup.cpu;
    ps->stream_len = (unsigned)bsn; ps->slice_count = (unsigned)nslices;
    if (eosMode & 2) { memcpy(ps->eos, eosNal, 16); ps->explicitEOSPresentFlag = 1; }
    ps->log2_max_pic_order_cnt_lsb_minus4 = sps->log2_max_pic_order_cnt_lsb_minus4;
    ps->delta_pic_order_always_zero_flag = sps->delta_pic_order_always_zero_flag;
    ps->frame_mbs_only_flag = sps->frame_mbs_only_flag;
    ps->PicWidthInMbs = (int)wmb; ps->FrameHeightInMbs = (int)hmb;
    ps->tileFormat = tileFormat; ps->gob_height = gob;
    ps->entropy_coding_mode_flag = pps->entropy_coding_mode_flag;
    ps->pic_order_present_flag = pps->bottom_field_pic_order_in_frame_present_flag;
    ps->num_ref_idx_l0_active_minus1 = pps->num_ref_idx_l0_default_active_minus1;
    ps->num_ref_idx_l1_active_minus1 = pps->num_ref_idx_l1_default_active_minus1;
    ps->deblocking_filter_control_present_flag = pps->deblocking_filter_control_present_flag;
    ps->redundant_pic_cnt_present_flag = pps->redundant_pic_cnt_present_flag;
    ps->transform_8x8_mode_flag = pps->transform_8x8_mode_flag;
    ps->pitch_luma = (unsigned)pitch; ps->pitch_chroma = (unsigned)pitch;
    ps->luma_top_offset = ps->luma_bot_offset = ps->luma_frame_offset = 0;
    ps->chroma_top_offset = ps->chroma_bot_offset = ps->chroma_frame_offset = 0;
    ps->HistBufferSize = (unsigned)(histSize >> 8);
    // DECODE1_MBHIST=<bytes>: mbhist_buffer_size (pictures wider than 1920 never started with 0)
    // pictures wider than 1920 use an MB history buffer (H264_SET_MBHIST_BUF_OFFSET + mbhist_buffer_size); without it
    // the engine read address 0 and the channel died with an MMU fault (Xid 31)
    ps->mbhist_buffer_size = (unsigned)mbhist.size;
    if (getenv("DECODE1_MBHIST")) ps->mbhist_buffer_size = (unsigned)strtoul(getenv("DECODE1_MBHIST"), NULL, 0);
    if (getenv("DECODE1_HISTSZ")) ps->HistBufferSize = (unsigned)strtoul(getenv("DECODE1_HISTSZ"), NULL, 0);
    ps->MbaffFrameFlag = sps->mb_adaptive_frame_field_flag && !first.field_pic_flag;
    ps->direct_8x8_inference_flag = sps->direct_8x8_inference_flag;
    ps->weighted_pred_flag = pps->weighted_pred_flag; ps->weighted_bipred_idc = pps->weighted_bipred_idc;
    ps->constrained_intra_pred_flag = pps->constrained_intra_pred_flag;
    ps->ref_pic_flag = first.nal_ref_idc != 0;
    ps->field_pic_flag = first.field_pic_flag; ps->bottom_field_flag = first.bottom_field_flag;
    ps->log2_max_frame_num_minus4 = sps->log2_max_frame_num_minus4;
    ps->chroma_format_idc = sps->chroma_format_idc;
    ps->pic_order_cnt_type = sps->pic_order_cnt_type;
    ps->pic_init_qp_minus26 = pps->pic_init_qp_minus26;
    ps->chroma_qp_index_offset = pps->chroma_qp_index_offset;
    ps->second_chroma_qp_index_offset = pps->second_chroma_qp_index_offset;
    ps->CurrPicIdx = 0; ps->CurrColIdx = 0; ps->frame_num = (unsigned)first.frame_num; ps->frame_surfaces = 1;
    H264PocState pst = { 0 }; int top, bot; h264_poc(&pst, sps, &first, &top, &bot);
    ps->CurrFieldOrderCnt[0] = top; ps->CurrFieldOrderCnt[1] = bot;
    for (int i = 0; i < 6; i++) for (int k = 0; k < 16; k++) ((uint8_t *)ps->WeightScale[i])[h264_zigzag4x4[k]] = pps->scaling4x4[i][k];
    for (int i = 0; i < 2; i++) for (int k = 0; k < 64; k++) ((uint8_t *)ps->WeightScale8x8[i])[h264_zigzag8x8[k]] = pps->scaling8x8[i][k];
    ps->qpprime_y_zero_transform_bypass_flag = sps->qpprime_y_zero_transform_bypass_flag;
    // DECODE1_TF=<TilingFormat>[:<OutStride>]: also let NVDEC's display DMA write the picture (T8.05 experiment)
    int tf = -1; unsigned outStride = (unsigned)(pitch >> 8);
    if (getenv("DECODE1_TF")) { sscanf(getenv("DECODE1_TF"), "%d:%u", &tf, &outStride); }
    if (tf >= 0) {
        ps->displayPara.enableTFOutput = 1; ps->displayPara.TilingFormat = (unsigned)tf; ps->displayPara.OutStride = outStride;
        ps->displayPara.OutputStructure = 0;
        ps->displayPara.OutputTop[0] = 0; ps->displayPara.OutputTop[1] = 0;
        printf("display DMA: TilingFormat %d OutStride %u\n", tf, outStride);
    }
    printf("setup: %zu bytes, POC %d/%d, tileFormat %u gob_height %u, pitch %zu, history %zu KB, coloc %zu KB\n",
           sizeof *ps, top, bot, tileFormat, gob, pitch, histSize >> 10, colocSize >> 10);

    // ---- methods ----
    chan_mthd(&c, 0, NVC56F_SET_OBJECT, 1, rm_class(&rm, RM_CLS_NVDEC));
    chan_mthd(&c, 0, NVC9B0_SET_APPLICATION_ID, 1, (NvU32)NVC9B0_SET_APPLICATION_ID_ID_H264);
    chan_mthd(&c, 0, NVC9B0_SET_CONTROL_PARAMS, 1, DRF_NUM(C9B0, _SET_CONTROL_PARAMS, _CODEC_TYPE, NVDEC_CODEC_H264) |
              DRF_NUM(C9B0, _SET_CONTROL_PARAMS, _GPTIMER_ON, 1));
    chan_mthd(&c, 0, NVC9B0_SET_DRV_PIC_SETUP_OFFSET, 1, hi(setup.gpu));
    chan_mthd(&c, 0, NVC9B0_SET_IN_BUF_BASE_OFFSET, 1, hi(bsb.gpu));
    chan_mthd(&c, 0, NVC9B0_SET_PICTURE_INDEX, 1, 0u);
    chan_mthd(&c, 0, NVC9B0_SET_SLICE_OFFSETS_BUF_OFFSET, 1, hi(offs.gpu));
    chan_mthd(&c, 0, NVC9B0_SET_COLOC_DATA_OFFSET, 1, hi(coloc.gpu));
    chan_mthd(&c, 0, NVC9B0_SET_HISTORY_OFFSET, 1, hi(hist.gpu));
    chan_mthd(&c, 0, NVC9B0_SET_NVDEC_STATUS_OFFSET, 1, hi(status.gpu));
    chan_mthd(&c, 0, NVC9B0_H264_SET_MBHIST_BUF_OFFSET, 1, hi(mbhist.gpu));
    if (tf >= 0) {
        chan_mthd(&c, 0, NVC9B0_SET_DISPLAY_BUF_SIZE, 1, (NvU32)dispL.size);
        chan_mthd(&c, 0, NVC9B0_SET_DISPLAY_BUF_LUMA_OFFSET, 1, hi(dispL.gpu));
        chan_mthd(&c, 0, NVC9B0_SET_DISPLAY_BUF_CHROMA_OFFSET, 1, hi(dispC.gpu));
    }
    if (!getenv("DECODE1_NO_SCRATCH")) {   // not needed up to 1080p; 4K faulted at address 0 without them
        chan_mthd(&c, 0, NVC9B0_SET_PIC_SCRATCH_BUF_OFFSET, 1, hi(scratch.gpu));
        chan_mthd(&c, 0, NVC9B0_SET_HISTOGRAM_OFFSET, 1, hi(histo.gpu));
    }
    chan_mthd(&c, 0, NVC9B0_SET_PICTURE_LUMA_OFFSET0, 1, hi(luma.gpu));
    chan_mthd(&c, 0, NVC9B0_SET_PICTURE_CHROMA_OFFSET0, 1, hi(chroma.gpu));
    chan_mthd(&c, 0, NVC9B0_EXECUTE, 1, 0u);
    NvU64 a = sem.gpu;
    chan_mthd(&c, 0, NVC9B0_SEMAPHORE_A, 3, (NvU32)(a >> 32), (NvU32)a, 0x5eed0001u);
    chan_mthd(&c, 0, NVC9B0_SEMAPHORE_D, 1, DRF_DEF(C9B0, _SEMAPHORE_D, _STRUCTURE_SIZE, _ONE) |
              DRF_DEF(C9B0, _SEMAPHORE_D, _OPERATION, _RELEASE) | DRF_DEF(C9B0, _SEMAPHORE_D, _PAYLOAD_SIZE, _32BIT));
    struct timespec t0, t; clock_gettime(CLOCK_MONOTONIC, &t0);
    chan_submit(&c);
    volatile NvU32 *sv = sem.cpu; double ms = 0;
    while (ms < 2000 && sv[0] != 0x5eed0001u) { clock_gettime(CLOCK_MONOTONIC, &t); ms = (t.tv_sec - t0.tv_sec) * 1e3 + (t.tv_nsec - t0.tv_nsec) / 1e6; }
    const nvdec_status_s *st = status.cpu;
    printf("after %.3f ms: semaphore %s, error notifier 0x%x; status: %u MBs ok, %u in error, %u cycles, error_status 0x%x, slice header error 0x%x\n",
           ms, sv[0] == 0x5eed0001u ? "ok" : "MISSING", chan_error(&c), st->mbs_correctly_decoded, st->mbs_in_error, st->cycle_count,
           st->error_status, st->slice_header_error_code);
    NvU64 fa; NvU32 ft; char fs[32];
    if (sv[0] != 0x5eed0001u && !chan_fault(&c, &fa, &ft, fs)) printf("MMU fault at 0x%llx type %u %s\n", (unsigned long long)fa, ft, fs);

    // ---- read back: untile as standard block-linear and compare ----
    if (getenv("DECODE1_WAIT_MS")) { struct timespec w = { 0, atoi(getenv("DECODE1_WAIT_MS")) * 1000000L }; nanosleep(&w, NULL); }
    FILE *f = fopen("/tmp/decode1-luma.raw", "wb"); fwrite(luma.cpu, 1, luma.size, f); fclose(f);
    f = fopen("/tmp/decode1-chroma.raw", "wb"); fwrite(chroma.cpu, 1, chroma.size, f); fclose(f);
    const unsigned cw = (unsigned)sps->width, ch = (unsigned)sps->height;
    if (rn < (size_t)cw * ch * 3 / 2) { printf("reference too small\n"); return 1; }
    uint8_t *y = malloc((size_t)cw * ch), *uv = malloc((size_t)cw * ch / 2), *u = malloc((size_t)cw * ch / 4), *v = malloc((size_t)cw * ch / 4);
    const uint8_t *L = luma.cpu, *C = chroma.cpu; size_t nonzero = 0;
    for (size_t i = 0; i < luma.size; i++) nonzero += L[i] != 0;
    // tileFormat 1 with gob_height 0 writes the layout measured for NVENC's input in T2.05d (64x16-byte tiles of 1 KB,
    // 16x4 sub-blocks ordered x16, y4, x32, y8): nvenc_in_offset. Others: standard block-linear (to be measured).
    #define OFF(x, r) (tileFormat == 1 ? nvenc_in_offset((x), (r), pitch) : bl_offset((x), (r), pitch, (unsigned)bh))
    for (unsigned r = 0; r < ch; r++) for (unsigned x = 0; x < cw; x++) y[(size_t)r * cw + x] = L[OFF(x, r)];
    for (unsigned r = 0; r < ch / 2; r++) for (unsigned x = 0; x < cw; x++) uv[(size_t)r * cw + x] = C[OFF(x, r)];
    for (size_t i = 0; i < (size_t)cw * ch / 4; i++) { u[i] = uv[2 * i]; v[i] = uv[2 * i + 1]; }
    const uint8_t *ry = ref, *ru = ref + (size_t)cw * ch, *rv = ru + (size_t)cw * ch / 4;
    printf("luma surface: %zu of %zu bytes non-zero\n", nonzero, (size_t)luma.size);
    if (tf >= 0) {   // the display buffers, read as pitch-linear NV12 with stride OutStride*256
        const uint8_t *DL = dispL.cpu, *DC = dispC.cpu; size_t nzl = 0, nzc = 0; const size_t ds = (size_t)outStride * 256;
        for (size_t i = 0; i < dispL.size; i++) nzl += DL[i] != 0;
        for (size_t i = 0; i < dispC.size; i++) nzc += DC[i] != 0;
        uint8_t *py = malloc((size_t)cw * ch);
        for (unsigned r = 0; r < ch; r++) memcpy(py + (size_t)r * cw, DL + (size_t)r * ds, cw);
        FILE *g = fopen("/tmp/decode1-disp-luma.raw", "wb"); fwrite(DL, 1, dispL.size, g); fclose(g);
        printf("RESULT display DMA TilingFormat %d: luma bytes written %zu, chroma %zu; as pitch-linear luma PSNR %.2f dB\n", tf, nzl, nzc,
               psnr(py, ry, (size_t)cw * ch));
    }
    printf("RESULT tileFormat %u gob %u: luma PSNR %.2f dB, chroma U %.2f V %.2f dB (99 = identical)\n", tileFormat, gob,
           psnr(y, ry, (size_t)cw * ch), psnr(u, ru, (size_t)cw * ch / 4), psnr(v, rv, (size_t)cw * ch / 4));
    return 0;
}
