/* test_fps_limit.c - the frame-rate limiter's rules. cc -I plugin tools/test_fps_limit.c -o /tmp/test_fps_limit && /tmp/test_fps_limit
   Each arm is a sentence about something the limiter must do or refuse. */
#include <stdio.h>
#include "nvmtl_fps_limit.h"
static int fails;
static void arm(const char *what, int ok) { printf("%s  %s\n", ok ? "ok  " : "FAIL", what); if (!ok) fails++; }
int main(void)
{
    arm("one number limits every application", nvmtl_fps_limit_for("60", "Game") == 60);
    arm("an empty or missing setting means unlimited", nvmtl_fps_limit_for("", "Game") == 0 && nvmtl_fps_limit_for(0, "Game") == 0);
    arm("0 means unlimited", nvmtl_fps_limit_for("0", "Game") == 0);
    arm("a value below 10 or above 1000 is a typing mistake and is ignored, not clamped",
        nvmtl_fps_limit_for("5", "Game") == 0 && nvmtl_fps_limit_for("1001", "Game") == 0 && nvmtl_fps_limit_for("60000000", "Game") == 0);
    arm("text after the number is rejected", nvmtl_fps_limit_for("60fps", "Game") == 0 && nvmtl_fps_limit_for("-60", "Game") == 0);
    arm("a per-application entry applies to that executable only",
        nvmtl_fps_limit_for("Game=60,Other=144", "Game") == 60 && nvmtl_fps_limit_for("Game=60,Other=144", "Other") == 144 && nvmtl_fps_limit_for("Game=60,Other=144", "Third") == 0);
    arm("the * entry is the default for applications not named", nvmtl_fps_limit_for("Game=60,*=90", "Third") == 90 && nvmtl_fps_limit_for("Game=60,*=90", "Game") == 60);
    arm("a named entry beats * wherever * appears", nvmtl_fps_limit_for("*=90,Game=60", "Game") == 60);
    arm("an executable whose name only starts with an entry's name is not matched", nvmtl_fps_limit_for("Game=60", "GameLauncher") == 0 && nvmtl_fps_limit_for("Game=60", "Gam") == 0);
    arm("a malformed entry costs only that entry", nvmtl_fps_limit_for("Game=abc,Other=120", "Other") == 120 && nvmtl_fps_limit_for("Game=abc,Other=120", "Game") == 0);
    arm("the window server and system processes are never limited, even by *=30 or a bare number",
        nvmtl_fps_limit_for("30", "WindowServer") == 0 && nvmtl_fps_limit_for("*=30", "WindowServer") == 0 && nvmtl_fps_limit_for("WindowServer=30", "WindowServer") == 0 && nvmtl_fps_limit_for("30", "") == 0);

    nvmtl_fps_pacer p; nvmtl_fps_pacer_init(&p, 100);   /* 10 ms */
    uint64_t t = 1000000000ull, slept = 0;
    arm("the first present never waits", nvmtl_fps_pace(&p, t) == 0);
    int frames = 1; uint64_t start = t;
    for (int i = 0; i < 99; i++) {                       /* an application that renders in 1 ms */
        t += 1000000; uint64_t w = nvmtl_fps_pace(&p, t); slept += w; t += w; frames++;
    }
    arm("a fast application is held to the limit: 100 frames take 990 ms to the last present (99 intervals)", t - start == 990000000ull);
    arm("it sleeps only the unused part of each interval", slept == 99ull * 9000000ull);
    nvmtl_fps_pacer_init(&p, 100); t = 5000000000ull; nvmtl_fps_pace(&p, t);
    t += 30000000; arm("a frame slower than the limit is not delayed further", nvmtl_fps_pace(&p, t) == 0);
    t += 1000000; uint64_t w1 = nvmtl_fps_pace(&p, t);
    arm("after a stall it restarts from now instead of firing a burst to catch up", w1 > 0 && w1 <= 10000000ull);
    nvmtl_fps_pacer_init(&p, 0);
    arm("a limit of 0 never waits", nvmtl_fps_pace(&p, 1) == 0 && nvmtl_fps_pace(&p, 2) == 0);
    printf(fails ? "FAILED %d\n" : "PASS all arms\n", fails);
    return fails != 0;
}
