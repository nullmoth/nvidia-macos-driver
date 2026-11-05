// av1probs.c: see av1probs.h.
#include "av1probs.h"
#include "av1tables.h"
#include <string.h>

typedef struct { uint8_t *t; int at, stride; } Put;
static Put put_at(uint8_t *t, int off, int stride) { return (Put){ t, off, stride }; }
static void put_vals(Put *p, const uint16_t *v, int n)   // one CDF of n values (the spec's CDF without 32768 and counter)
{
    for (int i = 0; i < n; i++) { const uint16_t x = (uint16_t)(32768 - v[i]); memcpy(p->t + p->at + 2 * i, &x, 2); }
    p->at += 2 * p->stride;
}
// count CDFs of a spec table whose CDFs are len entries long (the values, then 32768 and the counter)
static void put_spec(Put *p, const uint16_t *cdfs, int count, int len) { for (int k = 0; k < count; k++) put_vals(p, cdfs + k * len, len - 2); }
static void put_uniform(Put *p, int nsym, int count)   // the entries the spec does not define (never read)
{
    uint16_t v[16];
    for (int i = 1; i < nsym; i++) v[i - 1] = (uint16_t)((32768 * i + nsym / 2) / nsym);
    for (int k = 0; k < count; k++) put_vals(p, v, nsym - 1);
}
#define N(a) ((int)(sizeof(a) / sizeof((a)[0])))
#define SPEC(p, a) put_spec(p, (const uint16_t *)(a), (int)(sizeof(a) / sizeof(uint16_t)) / LEN(a), LEN(a))
#define LEN(a) lastdim_##a
// the length of the last dimension of each table used
#define LD(a, n) enum { lastdim_##a = n };
LD(Default_Partition_W8_Cdf, 5) LD(Default_Use_Wiener_Cdf, 3) LD(Default_Use_Sgrproj_Cdf, 3) LD(Default_Restoration_Type_Cdf, 4)
LD(Default_Partition_W16_Cdf, 11) LD(Default_Partition_W32_Cdf, 11) LD(Default_Partition_W64_Cdf, 11) LD(Default_Partition_W128_Cdf, 9)
LD(Default_Intra_Frame_Y_Mode_Cdf, 14) LD(Default_Segment_Id_Predicted_Cdf, 3) LD(Default_Segment_Id_Cdf, 9) LD(Default_Skip_Cdf, 3)
LD(Default_Delta_Q_Cdf, 5) LD(Default_Delta_Lf_Cdf, 5) LD(Default_Skip_Mode_Cdf, 3) LD(Default_Txfm_Split_Cdf, 3) LD(Default_Tx_8x8_Cdf, 3)
LD(Default_Tx_16x16_Cdf, 4) LD(Default_Tx_32x32_Cdf, 4) LD(Default_Tx_64x64_Cdf, 4) LD(Default_Y_Mode_Cdf, 14)
LD(Default_Uv_Mode_Cfl_Not_Allowed_Cdf, 14) LD(Default_Uv_Mode_Cfl_Allowed_Cdf, 15) LD(Default_Is_Inter_Cdf, 3) LD(Default_Comp_Mode_Cdf, 3)
LD(Default_Single_Ref_Cdf, 3) LD(Default_Comp_Ref_Type_Cdf, 3) LD(Default_Uni_Comp_Ref_Cdf, 3) LD(Default_Comp_Ref_Cdf, 3)
LD(Default_Comp_Bwd_Ref_Cdf, 3) LD(Default_New_Mv_Cdf, 3) LD(Default_Zero_Mv_Cdf, 3) LD(Default_Ref_Mv_Cdf, 3) LD(Default_Drl_Mode_Cdf, 3)
LD(Default_Interp_Filter_Cdf, 4) LD(Default_Mv_Joint_Cdf, 5) LD(Default_Mv_Sign_Cdf, 3) LD(Default_Mv_Class_Cdf, 12) LD(Default_Mv_Class0_Fr_Cdf, 5)
LD(Default_Mv_Fr_Cdf, 5) LD(Default_Mv_Class0_Hp_Cdf, 3) LD(Default_Mv_Hp_Cdf, 3) LD(Default_Mv_Class0_Bit_Cdf, 3) LD(Default_Mv_Bit_Cdf, 3)
LD(Default_Use_Obmc_Cdf, 3) LD(Default_Motion_Mode_Cdf, 4) LD(Default_Compound_Mode_Cdf, 9) LD(Default_Compound_Type_Cdf, 3)
LD(Default_Inter_Intra_Cdf, 3) LD(Default_Inter_Intra_Mode_Cdf, 5) LD(Default_Wedge_Inter_Intra_Cdf, 3) LD(Default_Wedge_Index_Cdf, 17)
LD(Default_Palette_Y_Mode_Cdf, 3) LD(Default_Palette_Uv_Mode_Cdf, 3) LD(Default_Palette_Y_Size_Cdf, 8) LD(Default_Palette_Uv_Size_Cdf, 8)
LD(Default_Cfl_Sign_Cdf, 9) LD(Default_Cfl_Alpha_Cdf, 17) LD(Default_Intrabc_Cdf, 3) LD(Default_Angle_Delta_Cdf, 8)
LD(Default_Filter_Intra_Mode_Cdf, 6) LD(Default_Filter_Intra_Cdf, 3) LD(Default_Comp_Group_Idx_Cdf, 3) LD(Default_Compound_Idx_Cdf, 3)
LD(Default_Intra_Tx_Type_Set1_Cdf, 8) LD(Default_Intra_Tx_Type_Set2_Cdf, 6) LD(Default_Inter_Tx_Type_Set1_Cdf, 17)
LD(Default_Inter_Tx_Type_Set2_Cdf, 13) LD(Default_Inter_Tx_Type_Set3_Cdf, 3)

void av1_default_probs(int q, uint8_t t[AV1_PROBS_B])
{
    memset(t, 0, AV1_PROBS_B);
    Put p;
    p = put_at(t, 0x0000, 3); SPEC(&p, Default_Partition_W8_Cdf);
    p = put_at(t, 0x0018, 1); SPEC(&p, Default_Use_Wiener_Cdf);
    p = put_at(t, 0x001a, 1); SPEC(&p, Default_Use_Sgrproj_Cdf);
    p = put_at(t, 0x001c, 2); SPEC(&p, Default_Restoration_Type_Cdf);
    p = put_at(t, 0x0020, 16); SPEC(&p, Default_Partition_W16_Cdf); SPEC(&p, Default_Partition_W32_Cdf); SPEC(&p, Default_Partition_W64_Cdf);
    p = put_at(t, 0x0132, 16); SPEC(&p, Default_Partition_W128_Cdf);   // in the space the 64x64 CDFs leave
    p = put_at(t, 0x01a0, 12); SPEC(&p, Default_Intra_Frame_Y_Mode_Cdf);
    p = put_at(t, 0x03f8, 1); SPEC(&p, Default_Segment_Id_Predicted_Cdf);
    p = put_at(t, 0x03fe, 7); SPEC(&p, Default_Segment_Id_Cdf);
    p = put_at(t, 0x0428, 1); SPEC(&p, Default_Skip_Cdf);
    p = put_at(t, 0x042e, 3); SPEC(&p, Default_Delta_Q_Cdf);
    p = put_at(t, 0x0434, 3); for (int i = 0; i < 4; i++) SPEC(&p, Default_Delta_Lf_Cdf);   // DeltaLFMulti: FRAME_LF_COUNT copies
    p = put_at(t, 0x044c, 3); SPEC(&p, Default_Delta_Lf_Cdf);
    p = put_at(t, 0x0452, 1); SPEC(&p, Default_Skip_Mode_Cdf);
    p = put_at(t, 0x0458, 1); SPEC(&p, Default_Txfm_Split_Cdf);
    p = put_at(t, 0x0484, 2); SPEC(&p, Default_Tx_8x8_Cdf);
    p = put_at(t, 0x0490, 2); SPEC(&p, Default_Tx_16x16_Cdf); SPEC(&p, Default_Tx_32x32_Cdf); SPEC(&p, Default_Tx_64x64_Cdf);
    p = put_at(t, 0x04b4, 12); SPEC(&p, Default_Y_Mode_Cdf);
    p = put_at(t, 0x0514, 13); SPEC(&p, Default_Uv_Mode_Cfl_Not_Allowed_Cdf);
    p = put_at(t, 0x0666, 13); SPEC(&p, Default_Uv_Mode_Cfl_Allowed_Cdf);
    p = put_at(t, 0x07b8, 1); SPEC(&p, Default_Is_Inter_Cdf);
    p = put_at(t, 0x07c0, 1); SPEC(&p, Default_Comp_Mode_Cdf);
    p = put_at(t, 0x07ca, 1); SPEC(&p, Default_Single_Ref_Cdf);
    p = put_at(t, 0x07ee, 1); SPEC(&p, Default_Comp_Ref_Type_Cdf);
    p = put_at(t, 0x07f8, 1); SPEC(&p, Default_Uni_Comp_Ref_Cdf);
    p = put_at(t, 0x080a, 1); SPEC(&p, Default_Comp_Ref_Cdf);
    p = put_at(t, 0x081c, 1); SPEC(&p, Default_Comp_Bwd_Ref_Cdf);
    p = put_at(t, 0x0828, 1); SPEC(&p, Default_New_Mv_Cdf);
    p = put_at(t, 0x0834, 1); SPEC(&p, Default_Zero_Mv_Cdf);
    p = put_at(t, 0x0838, 1); SPEC(&p, Default_Ref_Mv_Cdf);
    p = put_at(t, 0x084a, 1); SPEC(&p, Default_Drl_Mode_Cdf);
    p = put_at(t, 0x0850, 2); SPEC(&p, Default_Interp_Filter_Cdf);
    // motion vectors: per component, from the spec's context 0 (normal motion vectors)
    p = put_at(t, 0x0890, 3); SPEC(&p, Default_Mv_Joint_Cdf);
    p = put_at(t, 0x0896, 1); for (int c = 0; c < 2; c++) SPEC(&p, Default_Mv_Sign_Cdf);
    p = put_at(t, 0x089a, 10); for (int c = 0; c < 2; c++) put_spec(&p, Default_Mv_Class_Cdf[0], 1, 12);
    p = put_at(t, 0x08c2, 3); for (int c = 0; c < 2; c++) put_spec(&p, Default_Mv_Class0_Fr_Cdf[0][0], 2, 5);
    p = put_at(t, 0x08da, 3); for (int c = 0; c < 2; c++) put_spec(&p, Default_Mv_Fr_Cdf[0], 1, 5);
    p = put_at(t, 0x08e6, 1); for (int c = 0; c < 2; c++) SPEC(&p, Default_Mv_Class0_Hp_Cdf);
    p = put_at(t, 0x08ea, 1); for (int c = 0; c < 2; c++) SPEC(&p, Default_Mv_Hp_Cdf);
    p = put_at(t, 0x08ee, 1); for (int c = 0; c < 2; c++) SPEC(&p, Default_Mv_Class0_Bit_Cdf);
    p = put_at(t, 0x08f2, 1); for (int c = 0; c < 2; c++) SPEC(&p, Default_Mv_Bit_Cdf);
    p = put_at(t, 0x091a, 1); SPEC(&p, Default_Use_Obmc_Cdf);
    p = put_at(t, 0x0946, 2); SPEC(&p, Default_Motion_Mode_Cdf);
    p = put_at(t, 0x099e, 7); SPEC(&p, Default_Compound_Mode_Cdf);
    p = put_at(t, 0x0a70, 1); SPEC(&p, Default_Compound_Type_Cdf);
    p = put_at(t, 0x0a9c, 1); put_uniform(&p, 2, 1); SPEC(&p, Default_Inter_Intra_Cdf);          // block size group 0: unused
    p = put_at(t, 0x0aa4, 3); put_uniform(&p, 4, 1); SPEC(&p, Default_Inter_Intra_Mode_Cdf);
    p = put_at(t, 0x0abc, 1); SPEC(&p, Default_Wedge_Inter_Intra_Cdf);
    p = put_at(t, 0x0ae8, 15); SPEC(&p, Default_Wedge_Index_Cdf);
    p = put_at(t, 0x0d7c, 1); SPEC(&p, Default_Palette_Y_Mode_Cdf);
    p = put_at(t, 0x0da6, 1); SPEC(&p, Default_Palette_Uv_Mode_Cdf);
    p = put_at(t, 0x0daa, 6); SPEC(&p, Default_Palette_Y_Size_Cdf);
    p = put_at(t, 0x0dfe, 6); SPEC(&p, Default_Palette_Uv_Size_Cdf);
    p = put_at(t, 0x0e52, 7); SPEC(&p, Default_Cfl_Sign_Cdf);
    p = put_at(t, 0x0e60, 15); SPEC(&p, Default_Cfl_Alpha_Cdf);
    p = put_at(t, 0x0f14, 1); SPEC(&p, Default_Intrabc_Cdf);
    p = put_at(t, 0x0f16, 6); SPEC(&p, Default_Angle_Delta_Cdf);
    p = put_at(t, 0x0f76, 4); SPEC(&p, Default_Filter_Intra_Mode_Cdf);
    p = put_at(t, 0x0f7e, 1); SPEC(&p, Default_Filter_Intra_Cdf);
    p = put_at(t, 0x0faa, 1); SPEC(&p, Default_Comp_Group_Idx_Cdf);
    p = put_at(t, 0x0fb8, 1); SPEC(&p, Default_Compound_Idx_Cdf);
    // palette colour index CDFs: those of n and 10 - n colours share 8 values, those of 5 colours take 4
    for (int uv = 0; uv < 2; uv++) {
        const int base = uv ? 0x1100 : 0x0fe0;
        #define PAL(add, stride, n) do { p = put_at(t, base + (add), stride); \
            if (uv) put_spec(&p, (const uint16_t *)Default_Palette_Size_##n##_Uv_Color_Cdf, 5, n + 1); \
            else put_spec(&p, (const uint16_t *)Default_Palette_Size_##n##_Y_Color_Cdf, 5, n + 1); } while (0)
        PAL(0x00, 8, 2); PAL(0x02, 8, 8); PAL(0x50, 8, 3); PAL(0x54, 8, 7); PAL(0xa0, 8, 4); PAL(0xa6, 8, 6); PAL(0xf0, 4, 5);
        #undef PAL
    }
    // transform types: libaom's 4 transform sizes per set; the sizes a set is not used at hold uniform CDFs
    p = put_at(t, 0x1220, 8); SPEC(&p, Default_Intra_Tx_Type_Set1_Cdf); put_uniform(&p, 7, 2 * 13);
    p = put_at(t, 0x1560, 4); SPEC(&p, Default_Intra_Tx_Type_Set2_Cdf); put_uniform(&p, 5, 13);
    p = put_at(t, 0x1700, 16); SPEC(&p, Default_Inter_Tx_Type_Set1_Cdf); put_uniform(&p, 16, 2);
    p = put_at(t, 0x1780, 16); put_uniform(&p, 12, 2); SPEC(&p, Default_Inter_Tx_Type_Set2_Cdf); put_uniform(&p, 12, 1);
    p = put_at(t, 0x1796, 16); SPEC(&p, Default_Inter_Tx_Type_Set3_Cdf);   // after set 2's
    // coefficients, of quantizer context q
    p = put_at(t, 0x1800, 1); put_spec(&p, (const uint16_t *)Default_Txb_Skip_Cdf[q], 5 * 13, 3);
    p = put_at(t, 0x1882, 1); put_spec(&p, (const uint16_t *)Default_Eob_Extra_Cdf[q], 5 * 2 * 9, 3);
    p = put_at(t, 0x1940, 4); put_spec(&p, (const uint16_t *)Default_Eob_Pt_16_Cdf[q], 2 * 2, 6);
    p = put_at(t, 0x1960, 8); put_spec(&p, (const uint16_t *)Default_Eob_Pt_32_Cdf[q], 2 * 2, 7);
    p = put_at(t, 0x19a0, 8); put_spec(&p, (const uint16_t *)Default_Eob_Pt_64_Cdf[q], 2 * 2, 8);
    p = put_at(t, 0x19e0, 8); put_spec(&p, (const uint16_t *)Default_Eob_Pt_128_Cdf[q], 2 * 2, 9);
    p = put_at(t, 0x1a20, 8); put_spec(&p, (const uint16_t *)Default_Eob_Pt_256_Cdf[q], 2 * 2, 10);
    p = put_at(t, 0x1a60, 16); for (int pl = 0; pl < 2; pl++) { put_spec(&p, Default_Eob_Pt_512_Cdf[q][pl], 1, 11); put_uniform(&p, 10, 1); }
    p = put_at(t, 0x1ae0, 16); for (int pl = 0; pl < 2; pl++) { put_spec(&p, Default_Eob_Pt_1024_Cdf[q][pl], 1, 12); put_uniform(&p, 11, 1); }
    p = put_at(t, 0x1b60, 2); put_spec(&p, (const uint16_t *)Default_Coeff_Base_Eob_Cdf[q], 5 * 2 * 4, 4);
    p = put_at(t, 0x1c00, 4); put_spec(&p, (const uint16_t *)Default_Coeff_Base_Cdf[q], 5 * 2 * 42, 5);
    p = put_at(t, 0x2920, 1); put_spec(&p, (const uint16_t *)Default_Dc_Sign_Cdf[q], 2 * 3, 3);
    p = put_at(t, 0x2930, 4); put_spec(&p, (const uint16_t *)Default_Coeff_Br_Cdf[q], 5 * 2 * 21, 5);
}
