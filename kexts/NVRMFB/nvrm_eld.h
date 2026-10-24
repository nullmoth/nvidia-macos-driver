// nvrm_eld.h: build an ELD (EDID-Like Data) block from a display's EDID.
//
// The sink's ELD carries its Short Audio Descriptors (accepted formats, channels and sample rates), speaker allocation
// and monitor name. It describes the monitor's audio capabilities; AppleHDA also needs framebuffer display-to-codec
// association properties before it can create a display audio engine. This builds the ELD from the EDID the framebuffer
// already reads, so it can be published alongside that association.
//
// Format: HDMI ELD, "ELD_Ver" 2 (Intel HD Audio ELD / HDMI spec, matching Linux sound/pci/hda hdmi_eld). The SADs and
// the speaker-allocation byte are copied verbatim out of the EDID's CEA-861 extension block; the monitor name comes
// from the base block's 0xFC descriptor; the manufacturer and product words are the EDID's own bytes.
//
// Pure C with no libc or IOKit dependency at all (its own byte-zero loop) so it builds identically in the kext and in
// a standalone test. Every read of the caller-supplied EDID is bounds-checked: a truncated or malformed EDID yields 0
// (nothing published) rather than an out-of-bounds read.

#ifndef NVRM_ELD_H
#define NVRM_ELD_H

#include <stdint.h>
#include <stddef.h>

enum { NVRM_ELD_CONN_HDMI = 0, NVRM_ELD_CONN_DP = 1 };

// An ELD is at most: 4 (header) + 16 (fixed baseline) + 16 (monitor name, padded) + 15*3 (up to 15 SADs) = 81 bytes,
// rounded to a DWORD. 128 is a safe, aligned ceiling that also leaves room for conservative growth.
#define NVRM_ELD_MAX 128

// A CEA-861 data block's tag (top 3 bits of the block header byte).
enum { NVRM_CEA_TAG_AUDIO = 1, NVRM_CEA_TAG_SPEAKER = 4, NVRM_CEA_TAG_VENDOR = 3 };

struct nvrm_cea_scan {
    uint8_t sad[15 * 3];
    uint8_t sad_count;
    uint8_t speaker_alloc;
    uint8_t has_audio;
    uint8_t is_hdmi;
};

static inline void nvrm_scan_cea(const uint8_t *edid, size_t edid_len, struct nvrm_cea_scan *scan)
{
    for (size_t i = 0; i < sizeof *scan; i++) ((uint8_t *)scan)[i] = 0;
    if (!edid || edid_len < 128) return;

    for (size_t e = 1; e <= edid[126]; e++) {
        size_t base = (size_t)e * 128;
        if (base + 128 > edid_len) break;
        const uint8_t *cea = edid + base;
        if (cea[0] != 0x02 || cea[1] < 3) continue;
        uint8_t dtd = cea[2];
        if (dtd < 4 || dtd > 128) continue;
        size_t p = 4;
        while (p < dtd) {
            uint8_t hdr = cea[p];
            uint8_t tag = (uint8_t)(hdr >> 5), len = (uint8_t)(hdr & 0x1F);
            if (p + 1 + len > dtd) break;
            const uint8_t *body = cea + p + 1;
            if (tag == NVRM_CEA_TAG_AUDIO) {
                for (uint8_t i = 0; i + 3 <= len; i += 3) {
                    if (scan->sad_count >= 15) break;
                    scan->sad[scan->sad_count * 3 + 0] = body[i + 0];
                    scan->sad[scan->sad_count * 3 + 1] = body[i + 1];
                    scan->sad[scan->sad_count * 3 + 2] = body[i + 2];
                    scan->sad_count++;
                }
            } else if (tag == NVRM_CEA_TAG_SPEAKER && len >= 1) {
                scan->speaker_alloc = body[0];
            } else if (tag == NVRM_CEA_TAG_VENDOR && len >= 3 &&
                       body[0] == 0x03 && body[1] == 0x0C && body[2] == 0x00) {
                scan->is_hdmi = 1;
            }
            p += 1 + len;
        }
    }
    scan->has_audio = scan->sad_count != 0;
}
// Returns the number of bytes written to out (a multiple of 4), or 0 if the EDID is too short/malformed to describe a
// sink. out must hold at least NVRM_ELD_MAX bytes. conn_type is NVRM_ELD_CONN_HDMI or NVRM_ELD_CONN_DP.
static inline size_t nvrm_build_eld(const uint8_t *edid, size_t edid_len, int conn_type, uint8_t *out)
{
    if (!edid || !out || edid_len < 128) return 0;             // no base block, nothing to describe

    // --- base block: manufacturer name (bytes 8 and 9), product code (bytes 10 and 11), monitor name (0xFC descriptor) ---
    uint8_t mfr0 = edid[8], mfr1 = edid[9], prod0 = edid[10], prod1 = edid[11];

    // The four 18-byte descriptors live at 54, 72, 90, 108. A display-product-name descriptor has the 5-byte prefix
    // 00 00 00 FC 00 and up to 13 name bytes, terminated by 0x0A and padded with spaces.
    char name[14]; uint8_t mnl = 0;
    for (int d = 0; d < 4 && mnl == 0; d++) {
        size_t o = (size_t)(54 + d * 18);
        if (o + 18 > 128) break;
        if (edid[o] == 0 && edid[o+1] == 0 && edid[o+2] == 0 && edid[o+3] == 0xFC && edid[o+4] == 0) {
            for (int i = 0; i < 13; i++) {
                uint8_t ch = edid[o + 5 + (size_t)i];
                if (ch == 0x0A) break;                          // EDID line terminator
                name[mnl++] = (char)ch;
            }
            while (mnl > 0 && name[mnl-1] == ' ') mnl--;         // trim the space padding
        }
    }
    if (mnl > 13) mnl = 13;

    // --- CEA-861 extension block: Short Audio Descriptors and the speaker-allocation byte ---
    struct nvrm_cea_scan cea;
    nvrm_scan_cea(edid, edid_len, &cea);
    // The HDMI VSDB marks an HDMI sink attached through a DisplayPort adapter.
    if (cea.is_hdmi) conn_type = NVRM_ELD_CONN_HDMI;
    conn_type &= 0x3;

    // --- assemble ---
    // Baseline block = 4 (ver/sad/conn/synch) + 8 (port id) + 2 (mfr) + 2 (product) + mnl + 3*sad_count, padded to 4.
    size_t baseline = 16u + (size_t)mnl + (size_t)cea.sad_count * 3u;
    size_t pad = (4u - (baseline & 3u)) & 3u;
    size_t baseline_padded = baseline + pad;
    size_t total = 4u + baseline_padded;
    if (total > NVRM_ELD_MAX) return 0;                         // cannot happen with the clamps above; defensive

    for (size_t i = 0; i < total; i++) out[i] = 0;
    out[0] = (uint8_t)(2u << 3);                                // ELD_Ver = 2 in bits [7:3]
    out[2] = (uint8_t)(baseline_padded / 4u);                  // baseline length in DWORDs
    uint8_t *bl = out + 4;
    bl[0] = (uint8_t)((1u << 5) | (mnl & 0x1F));               // CEA_EDID_Ver = 1, MNL
    bl[1] = (uint8_t)(((cea.sad_count & 0x0F) << 4) | ((conn_type & 0x3) << 2));  // SAD count, conn type, S_AI/HDCP = 0
    bl[2] = 0;                                                  // aud_synch_delay: unknown, 0 = no added delay
    bl[3] = (uint8_t)(cea.speaker_alloc & 0x7F);
    // bl[4..11] port id: left 0 (a fixed per-port id is optional; the pairing is carried by hda-gfx, not the ELD)
    bl[12] = mfr0; bl[13] = mfr1;
    bl[14] = prod0; bl[15] = prod1;
    for (uint8_t i = 0; i < mnl; i++) bl[16 + i] = (uint8_t)name[i];
    for (uint8_t i = 0; i < cea.sad_count * 3; i++) bl[16 + mnl + i] = cea.sad[i];
    return total;
}

#endif
