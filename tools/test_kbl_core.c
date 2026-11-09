/* test_kbl_core.c - the keyboard backlight driver's kernel-free rules. cc -I kexts/NMKbdBacklight tools/test_kbl_core.c -o /tmp/t && /tmp/t */
#include <stdio.h>
#include "kbl_core.h"
static int fails;
static void arm(const char *what, int ok) { printf("%s  %s\n", ok ? "ok  " : "FAIL", what); if (!ok) fails++; }
int main(void)
{
    /* the value the ASUS table returns for a present light at level 2 (device id | flags | 0x80 << 8 | level) */
    uint32_t dsts = 0x00050000u | 0x00200000u | 0x00100000u | (0x80u << 8) | 2u;
    arm("a state with the presence bit is present", kbl_asus_present(dsts));
    arm("the level is the low seven bits", kbl_asus_level(dsts) == 2);
    arm("the unsupported answer is not present", !kbl_asus_present(KBL_ASUS_UNSUPPORTED) && !kbl_asus_present(0xFFFFFFFFu));
    arm("a state without the presence bit is not present", !kbl_asus_present(0x00000002u));
    arm("a level above the maximum is clamped", kbl_asus_level(0x00010009u) == 3);
    arm("off is argument 0", kbl_asus_set_argument(0) == 0 && kbl_asus_set_argument(-1) == 0);
    arm("a level is 0x80 | level", kbl_asus_set_argument(1) == 0x81 && kbl_asus_set_argument(3) == 0x83);
    arm("a level above the maximum is set as the maximum", kbl_asus_set_argument(9) == 0x83);
    arm("0 maps to level 0 and the largest value to the maximum", kbl_smc_to_level(0, 3) == 0 && kbl_smc_to_level(0xFFF, 3) == 3);
    uint8_t b0, b1;
    kbl_smc_encode(0xABC, &b0, &b1);
    arm("a 12-bit brightness is byte 0 = bits 11..4 and the high nibble of byte 1 = bits 3..0 (Apple's setLEDBrightness)", b0 == 0xAB && b1 == 0xC0);
    arm("the word Apple writes, ((v & 0xF) << 12) | ((v >> 4) & 0xFF) little-endian, is the same two bytes",
        (uint8_t)((((0xABC & 0xF) << 12) | ((0xABC >> 4) & 0xFF)) & 0xFF) == b0 && (uint8_t)((((0xABC & 0xF) << 12) | ((0xABC >> 4) & 0xFF)) >> 8) == b1);
    arm("decoding gives the value back", kbl_smc_decode(b0, b1) == 0xABC);
    int ok = 1;
    for (int l = 0; l <= 3; l++) {
        kbl_smc_encode(kbl_level_to_smc(l, 3), &b0, &b1);
        if (kbl_smc_to_level(kbl_smc_decode(b0, b1), 3) != l) ok = 0;
    }
    arm("every level survives a round trip through the two SMC bytes", ok);
    ok = 1;
    for (int v = 0; v <= 0xFFF; v++) { int l = kbl_smc_to_level(v, 3); if (l < 0 || l > 3) ok = 0; }
    arm("no 12-bit value maps outside the levels", ok);
    arm("the method ids are the ASCII of their names", KBL_ASUS_METHOD_DSTS == ('D' | 'S' << 8 | 'T' << 16 | 'S' << 24)
                                                    && KBL_ASUS_METHOD_DEVS == ('D' | 'E' << 8 | 'V' << 16 | 'S' << 24));
    return fails ? 1 : 0;
}
