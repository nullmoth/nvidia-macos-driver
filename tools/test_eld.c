// test_eld.c: nvrm_build_eld() against hand-built EDIDs.
//
//   cc -std=c11 -Wall -Wextra -I../kexts/NVRMFB -o /tmp/test_eld test_eld.c && /tmp/test_eld
//
// Negative controls that must FIRE (a stub builder that ignored the CEA block, or dropped bounds checks, fails them):
//   - an EDID with an audio data block must yield exactly those SADs (a baseline-only builder yields 0 and fails);
//   - a truncated EDID, and a CEA block whose extension bytes are absent, must be refused / yield 0 SADs, not read OOB;
//   - the speaker-allocation and monitor name must come through.

#include "nvrm_eld.h"
#include <stdio.h>
#include <string.h>

static int g_fail;
#define CHECK(cond, msg) do { if (!(cond)) { printf("  FAIL: %s\n", msg); g_fail++; } } while (0)

// A minimal valid base block with manufacturer/product bytes and a 0xFC monitor-name descriptor "NVTEST".
static void base_block(uint8_t e[128], uint8_t ext_count)
{
    memset(e, 0, 128);
    // header 00 FF FF FF FF FF FF 00
    e[0] = 0x00; for (int i = 1; i <= 6; i++) e[i] = 0xFF; e[7] = 0x00;
    e[8] = 0x12; e[9] = 0x34;            // manufacturer id bytes
    e[10] = 0xAB; e[11] = 0xCD;          // product code bytes
    // 0xFC display-product-name descriptor at offset 54
    size_t o = 54;
    e[o+0] = 0; e[o+1] = 0; e[o+2] = 0; e[o+3] = 0xFC; e[o+4] = 0;
    const char *nm = "NVTEST";
    for (size_t i = 0; i < strlen(nm); i++) e[o+5+i] = (uint8_t)nm[i];
    e[o+5+strlen(nm)] = 0x0A;            // terminator
    for (size_t i = o+5+strlen(nm)+1; i < o+18; i++) e[i] = ' ';
    e[126] = ext_count;
    // checksum byte 127 is not validated by the builder; leave 0
}

// A CEA-861 rev-3 extension block carrying an audio data block (two SADs), a speaker-allocation block, and an HDMI VSDB.
static void cea_block(uint8_t c[128])
{
    memset(c, 0, 128);
    c[0] = 0x02;            // CEA-861 tag
    c[1] = 0x03;            // revision 3
    size_t p = 4;
    // Audio Data Block: tag 1, two 3-byte SADs (LPCM 2ch 48/44.1kHz 16-bit; AC-3 6ch).
    c[p++] = (uint8_t)((NVRM_CEA_TAG_AUDIO << 5) | 6);
    uint8_t sad_lpcm[3] = { 0x09, 0x07, 0x07 };
    uint8_t sad_ac3[3]  = { 0x15, 0x07, 0x50 };
    memcpy(c + p, sad_lpcm, 3); p += 3;
    memcpy(c + p, sad_ac3, 3);  p += 3;
    // Speaker Allocation Data Block: tag 4, 3 bytes; first byte = FL/FR + LFE + FC = 0x07.
    c[p++] = (uint8_t)((NVRM_CEA_TAG_SPEAKER << 5) | 3);
    c[p++] = 0x07; c[p++] = 0x00; c[p++] = 0x00;
    // Vendor-specific (HDMI) Data Block: tag 3, OUI 00-0C-03 then two bytes of phys addr.
    c[p++] = (uint8_t)((NVRM_CEA_TAG_VENDOR << 5) | 5);
    c[p++] = 0x03; c[p++] = 0x0C; c[p++] = 0x00; c[p++] = 0x10; c[p++] = 0x00;
    c[2] = (uint8_t)p;      // DTD offset: data blocks end here
}

int main(void)
{
    uint8_t out[NVRM_ELD_MAX];

    // 1. EDID with a CEA audio block: the SADs, speaker allocation and name must come through.
    printf("case: EDID with CEA audio block\n");
    {
        uint8_t edid[256]; base_block(edid, 1); cea_block(edid + 128);
        size_t n = nvrm_build_eld(edid, sizeof edid, NVRM_ELD_CONN_HDMI, out);
        CHECK(n > 0, "builder returned something");
        CHECK((n & 3) == 0, "ELD length is a multiple of 4");
        CHECK(out[0] == (2u << 3), "ELD_Ver == 2");
        CHECK(out[2] == (n - 4) / 4, "baseline length in DWORDs matches");
        uint8_t *bl = out + 4;
        uint8_t mnl = bl[0] & 0x1F;
        uint8_t sad_count = bl[1] >> 4;
        uint8_t conn = (bl[1] >> 2) & 3;
        CHECK(sad_count == 2, "two SADs recovered (baseline-only stub fails here)");
        CHECK(conn == NVRM_ELD_CONN_HDMI, "connection type HDMI");
        CHECK(bl[3] == 0x07, "speaker allocation byte copied");
        CHECK(bl[12] == 0x12 && bl[13] == 0x34, "manufacturer bytes");
        CHECK(bl[14] == 0xAB && bl[15] == 0xCD, "product bytes");
        CHECK(mnl == 6 && memcmp(bl + 16, "NVTEST", 6) == 0, "monitor name NVTEST");
        // the exact SAD bytes, verbatim, right after the name
        uint8_t *sads = bl + 16 + mnl;
        CHECK(sads[0] == 0x09 && sads[1] == 0x07 && sads[2] == 0x07, "SAD #0 (LPCM) verbatim");
        CHECK(sads[3] == 0x15 && sads[4] == 0x07 && sads[5] == 0x50, "SAD #1 (AC-3) verbatim");
        // total size == 4 + 16 + mnl + 6, padded to 4
        size_t baseline = 16u + mnl + 6u; size_t want = 4u + ((baseline + 3u) & ~3u);
        CHECK(n == want, "total length accounts for header + baseline + name + SADs + padding");
    }

    // 2. NEGATIVE CONTROL: DisplayPort sink with NO CEA extension. No SADs, no speaker map, but a valid baseline+name.
    //    A builder that fabricated SADs, or that ignored conn_type, fails here.
    printf("case: EDID with no CEA extension (DP)\n");
    {
        uint8_t edid[128]; base_block(edid, 0);
        size_t n = nvrm_build_eld(edid, sizeof edid, NVRM_ELD_CONN_DP, out);
        CHECK(n > 0, "still builds a baseline ELD");
        uint8_t *bl = out + 4;
        CHECK((bl[1] >> 4) == 0, "zero SADs when the sink lists none");
        CHECK(((bl[1] >> 2) & 3) == NVRM_ELD_CONN_DP, "DP connection type preserved (no HDMI VSDB to override)");
        CHECK(bl[3] == 0, "no speaker allocation");
        CHECK((bl[0] & 0x1F) == 6, "monitor name length still 6");
    }

    // 3. NEGATIVE CONTROL: extension count claims a CEA block that is NOT in the buffer. Must not read past the end;
    //    must fall back to a baseline ELD with 0 SADs rather than trusting the absent bytes.
    printf("case: EDID claims an extension that is absent (bounds)\n");
    {
        uint8_t edid[128]; base_block(edid, 1);   // says 1 extension, but the buffer is only 128 bytes
        size_t n = nvrm_build_eld(edid, sizeof edid, NVRM_ELD_CONN_HDMI, out);
        CHECK(n > 0, "builds despite the missing extension");
        CHECK((out[4+1] >> 4) == 0, "no SADs invented from out-of-bounds memory");
    }

    // 4. NEGATIVE CONTROL: a truncated EDID (under one block) must be refused outright.
    printf("case: truncated EDID\n");
    {
        uint8_t edid[64]; memset(edid, 0, sizeof edid);
        size_t n = nvrm_build_eld(edid, sizeof edid, NVRM_ELD_CONN_HDMI, out);
        CHECK(n == 0, "a sub-128-byte EDID yields nothing");
        CHECK(nvrm_build_eld(NULL, 0, NVRM_ELD_CONN_HDMI, out) == 0, "NULL EDID refused");
    }

    // 5. HDMI VSDB overrides a DP guess (a DP port carrying an HDMI dongle reports an HDMI VSDB).
    printf("case: HDMI VSDB overrides a DP guess\n");
    {
        uint8_t edid[256]; base_block(edid, 1); cea_block(edid + 128);
        size_t n = nvrm_build_eld(edid, sizeof edid, NVRM_ELD_CONN_DP, out);
        CHECK(n > 0, "builds");
        CHECK(((out[4+1] >> 2) & 3) == NVRM_ELD_CONN_HDMI, "VSDB promoted the guess to HDMI");
    }

    printf(g_fail ? "\n%d assertion(s) FAILED\n" : "\nall assertions passed\n", g_fail);
    return g_fail ? 1 : 0;
}
