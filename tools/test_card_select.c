/* Host test for plugin/nvmtl_card_select.h. cc -I plugin tools/test_card_select.c -o /tmp/t && /tmp/t */
#include <stdio.h>
#include "nvmtl_card_select.h"
static int fails;
static void arm(const char *w, int ok) { printf("%s  %s\n", ok ? "ok  " : "FAIL", w); if (!ok) fails++; }
int main(void)
{
    arm("without nvmultigpu every accelerator is accepted whatever the setting says (today's behaviour)",
        nvmtl_card_accept(0, -1, 0, 3) && nvmtl_card_accept(0, 2, 0, 0) && nvmtl_card_accept(0, -1, -1, 0));
    arm("multi, no assignment: only the card the kernel marked primary is accepted",
        nvmtl_card_accept(1, -1, 1, 1) && !nvmtl_card_accept(1, -1, 0, 0));
    arm("multi, kernel silent about primary: the first accelerator, as before", nvmtl_card_accept(1, -1, -1, 0) && !nvmtl_card_accept(1, -1, -1, 1));
    arm("multi, an assignment picks that ordinal and refuses the others, the primary included",
        nvmtl_card_accept(1, 1, 0, 1) && !nvmtl_card_accept(1, 1, 1, 0) && !nvmtl_card_accept(1, 1, 0, 2));
    int accepted = 0; for (int o = 0; o < 4; o++) accepted += nvmtl_card_accept(1, 2, o == 0, o);
    arm("multi, four cards: exactly one is accepted for an assignment, and exactly one with none",
        accepted == 1 && ({ int n = 0; for (int o = 0; o < 4; o++) n += nvmtl_card_accept(1, -1, o == 0, o); n; }) == 1);
    arm("one number applies to every application", nvmtl_card_for("1", "Game") == 1 && nvmtl_card_for("0", "Game") == 0);
    arm("a list assigns by executable, with * as the default", nvmtl_card_for("Game=1,*=0", "Game") == 1 && nvmtl_card_for("Game=1,*=0", "Other") == 0 && nvmtl_card_for("Game=1", "Other") == -1);
    arm("a named entry beats * wherever it appears", nvmtl_card_for("*=0,Game=2", "Game") == 2);
    arm("an executable that only starts like an entry is not matched", nvmtl_card_for("Game=1", "GameLauncher") == -1);
    arm("a number above 7, text, or an empty setting is no assignment",
        nvmtl_card_for("8", "G") == -1 && nvmtl_card_for("x", "G") == -1 && nvmtl_card_for("", "G") == -1 && nvmtl_card_for("G=9", "G") == -1 && nvmtl_card_for("G=ab", "G") == -1);
    arm("a malformed entry costs only that entry", nvmtl_card_for("G=x,H=2", "H") == 2 && nvmtl_card_for("G=x,H=2", "G") == -1);
    {   /* the single-vendor path is unchanged: with NVIDIA cards only, the ordinal is the sorted position the old code computed */
        unsigned long long ids[3] = { 33, 7, 21 };
        int old[3];
        for (int i = 0; i < 3; i++) { int o = 0; for (int j = 0; j < 3; j++) if (ids[j] < ids[i]) o++; old[i] = o; }
        arm("NVIDIA cards only: ordinals equal the old sorted position, and an NVAccelPrimary-less machine still takes the first",
            nvmtl_card_ordinal(ids, 3, 33) == old[0] && nvmtl_card_ordinal(ids, 3, 7) == old[1] && nvmtl_card_ordinal(ids, 3, 21) == old[2]
            && nvmtl_card_accept(1, -1, -1, nvmtl_card_ordinal(ids, 3, 7)) && !nvmtl_card_accept(1, -1, -1, nvmtl_card_ordinal(ids, 3, 21)));
        arm("an AMD/Intel GPU alone (no NVIDIA card) is the first accelerator and is accepted with no assignment",
            nvmtl_card_accept(1, -1, nvmtl_card_nmx_primary(0), nvmtl_card_ordinal(ids, 1, 33)));
    }
    {   /* an NVIDIA card (id 40) with two AMD/Intel GPUs NMXGPU drives (ids 10 and 90) */
        unsigned long long ids[3] = { 90, 40, 10 };
        arm("one order across both classes: the AMD/Intel GPU with the lowest id is card 0, the NVIDIA card 1, the other GPU 2",
            nvmtl_card_ordinal(ids, 3, 10) == 0 && nvmtl_card_ordinal(ids, 3, 40) == 1 && nvmtl_card_ordinal(ids, 3, 90) == 2);
        arm("an id not in the list is card 0, as the NVIDIA-only code did", nvmtl_card_ordinal(ids, 3, 77) == 0 && nvmtl_card_ordinal(ids, 0, 5) == 0);
        arm("with an NVIDIA card present an NMXAccel is never the primary; alone it falls back to the first accelerator",
            nvmtl_card_nmx_primary(1) == 0 && nvmtl_card_nmx_primary(2) == 0 && nvmtl_card_nmx_primary(0) == -1);
        int n = 0, got = -1;
        for (int i = 0; i < 3; i++) { int prim = (ids[i] == 40) ? 1 : nvmtl_card_nmx_primary(1);
            if (nvmtl_card_accept(1, -1, prim, nvmtl_card_ordinal(ids, 3, ids[i]))) { n++; got = (int)ids[i]; } }
        arm("multi, mixed vendors, no assignment: exactly one card, the NVIDIA primary, is accepted", n == 1 && got == 40);
        n = 0; got = -1;
        for (int i = 0; i < 3; i++) { int prim = (ids[i] == 40) ? 1 : nvmtl_card_nmx_primary(1);
            if (nvmtl_card_accept(1, 0, prim, nvmtl_card_ordinal(ids, 3, ids[i]))) { n++; got = (int)ids[i]; } }
        arm("multi, mixed vendors, NVMTL_CARD=0: exactly one card, the AMD/Intel GPU with the lowest id", n == 1 && got == 10);
        n = 0;
        for (int i = 0; i < 3; i++) n += nvmtl_card_accept(0, 2, i == 1, nvmtl_card_ordinal(ids, 3, ids[i]));
        arm("without nvmultigpu=1 every accelerator is accepted, whatever the vendors", n == 3);
    }
    {   /* two GPUs of one model: told apart by PCI location */
        unsigned bus, dev, fn; char tag[32];
        arm("the registry's location string \"3:0:0\" parses to bus 3, device 0, function 0",
            nvmtl_pci_loc_parse("3:0:0", &bus, &dev, &fn) && bus == 3 && dev == 0 && fn == 0);
        arm("\"66:1:7\" is bus 66 (0x42), device 1, function 7 and its tag is pci-0000_42_01_7",
            nvmtl_pci_loc_parse("66:1:7", &bus, &dev, &fn) && (nvmtl_pci_tag(tag, sizeof tag, bus, dev, fn), !strcmp(tag, "pci-0000_42_01_7")));
        arm("the tag has the length Mesa's device-select layer compares (pci-xxxx_yy_zz_w)", strlen(tag) == strlen("pci-xxxx_yy_zz_w"));
        arm("malformed locations are refused: empty, text, two fields, four fields, trailing colon, device above 31, function above 7, bus above 255",
            !nvmtl_pci_loc_parse("", &bus, &dev, &fn) && !nvmtl_pci_loc_parse(NULL, &bus, &dev, &fn) && !nvmtl_pci_loc_parse("a:b:c", &bus, &dev, &fn)
            && !nvmtl_pci_loc_parse("3:0", &bus, &dev, &fn) && !nvmtl_pci_loc_parse("1:2:3:4", &bus, &dev, &fn) && !nvmtl_pci_loc_parse("3:0:", &bus, &dev, &fn)
            && !nvmtl_pci_loc_parse("3:32:0", &bus, &dev, &fn) && !nvmtl_pci_loc_parse("3:0:8", &bus, &dev, &fn) && !nvmtl_pci_loc_parse("256:0:0", &bus, &dev, &fn));
        arm("a single GPU of its model keeps today's \"vendor:device\" selection, with or without a location",
            !nvmtl_gpu_use_pci_tag(1, 1) && !nvmtl_gpu_use_pci_tag(1, 0) && !nvmtl_gpu_use_pci_tag(0, 1));
        arm("two GPUs of one model use the PCI tag when the location is known, and fall back to \"vendor:device\" when it is not",
            nvmtl_gpu_use_pci_tag(2, 1) && nvmtl_gpu_use_pci_tag(3, 1) && !nvmtl_gpu_use_pci_tag(2, 0));
    }
    printf(fails ? "FAILED %d\n" : "PASS all arms\n", fails);
    return fails != 0;
}
