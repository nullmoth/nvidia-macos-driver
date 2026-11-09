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
    printf(fails ? "FAILED %d\n" : "PASS all arms\n", fails);
    return fails != 0;
}
