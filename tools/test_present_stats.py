#!/usr/bin/env python3
"""The framebuffer reports what the compositor's presents did, so a "windows stutter, the cursor is smooth" report is decided
by the log: flips per second against the panel rate, flips past one refresh period, presents skipped for a flip still
pending, and the longest completion wait. The real helper is lifted from nvrm-fb.cpp and run on a fake clock.
Arms: a 165 Hz drag prints one line at the full rate; the same drag with every other flip late prints the late and over
counts; an idle desktop prints nothing; the line count is capped; the window restarts after each line. Two negative
controls must fail: a helper that prints idle windows, and one that never restarts its window."""
import pathlib
import subprocess
import tempfile

root = pathlib.Path(__file__).resolve().parents[1]
source = (root / 'kexts/NVRMFB/fb/nvrm-fb.cpp').read_text()

start = source.index('struct NVPresentWindow {')
end = source.index('\n}\n', source.index('static bool presentStatsNote(')) + 3
helper = source[start:end]
assert '"present stats head %u' in helper, 'the helper no longer builds the present statistics line'
for name in ('presentStatsNote(fHead, flipWaitUs(), true, false, false,', 'presentStatsNote(fHead, flipWaitUs(), true, true, false,',
             'presentStatsNote(fHead, flipWaitUs(), false, false, true, 0)'):
    assert source.count(name) == 1, 'submitConfig no longer reports: ' + name

idle_control = helper.replace('const bool busy = w.flips >= 60 || w.over || w.late;', 'const bool busy = true;')
assert idle_control != helper
nowindow_control = helper.replace('    w = NVPresentWindow();\n', '')
assert nowindow_control != helper

fixture = r'''
#include <cassert>
#include <cstdint>
#include <cstdio>
#include <cstring>
#include <string>
#include <vector>
#include <cstdarg>
static std::vector<std::string> gLines;
static uint64_t gClock = 1000;   // nanoseconds, the absolute-time unit here
static uint64_t mach_absolute_time() { return gClock; }
static void absolutetime_to_nanoseconds(uint64_t t, uint64_t *ns) { *ns = t; }
HELPER
// One compositor second: n frame-path presents spread evenly; every `lateEvery`-th is skipped late, every `overEvery`-th
// is a flip that missed its vblank.
static void run(unsigned head, double seconds, unsigned perSec, unsigned boundUs, unsigned lateEvery, unsigned overEvery) {
    const unsigned n = (unsigned)(seconds * perSec);
    for (unsigned i = 0; i < n; i++) {
        gClock += (uint64_t)(1e9 / perSec);
        const bool late = lateEvery && i % lateEvery == 0, over = overEvery && i % overEvery == 0;
        bool say;
        if (late) say = presentStatsNote(head, boundUs, false, false, true, 0);
        else if (over) say = presentStatsNote(head, boundUs, true, true, false, boundUs);
        else say = presentStatsNote(head, boundUs, true, false, false, 3000);
        if (say) gLines.push_back(gPresentMsg);
    }
}
int main() {
    const unsigned bound165 = 7575;   // flipWaitUs() at 165 Hz: one period and a quarter
    // A 165 Hz drag: one line per 10 s window, at the panel rate, no misses.
    { gLines.clear(); gClock = 1000; memset(gPresentWin, 0, sizeof gPresentWin); memset(gPresentLines, 0, sizeof gPresentLines);
      run(0, 10.2, 165, bound165, 0, 0);
      printf("165 Hz drag: %zu line(s): %s\n", gLines.size(), gLines.empty() ? "" : gLines[0].c_str());
      assert(gLines.size() == 1 && "a busy 10 s window prints exactly one line");
      assert(gLines[0].find("(165/s, panel 165 Hz)") != std::string::npos && "the line names the rate and the panel rate");
      assert(gLines[0].find("0 not on the panel within a refresh period and a quarter, 0 presents skipped") != std::string::npos); }
    // The same drag at half rate with misses: the counts are in the line.
    { gLines.clear(); gClock = 1000; memset(gPresentWin, 0, sizeof gPresentWin); memset(gPresentLines, 0, sizeof gPresentLines);
      run(0, 10.2, 80, bound165, 4, 5);
      printf("stuttering drag: %s\n", gLines.empty() ? "(none)" : gLines[0].c_str());
      assert(gLines.size() == 1);
      assert(gLines[0].find("panel 165 Hz") != std::string::npos && gLines[0].find("(60/s") != std::string::npos);
      assert(gLines[0].find("longest completion wait 7575 us") != std::string::npos);
      assert(gLines[0].find(" 0 presents skipped") == std::string::npos && gLines[0].find(" 0 not on the panel") == std::string::npos &&
             "late and over presents must be counted"); }
    // An idle desktop (3 flips in the window) prints nothing.
    { gLines.clear(); gClock = 1000; memset(gPresentWin, 0, sizeof gPresentWin); memset(gPresentLines, 0, sizeof gPresentLines);
      run(0, 10.2, 1, bound165, 0, 0); gLines.clear(); run(0, 10.5, 1, bound165, 0, 0);
      assert(gLines.empty() && "a quiet window must print nothing"); }
    // Five seconds of smooth dragging, a minute of nothing, then a drag: the pause starts the window over, so the line
    // reports the drag's rate and not the flips diluted over the idle minute.
    { gLines.clear(); gClock = 1000; memset(gPresentWin, 0, sizeof gPresentWin); memset(gPresentLines, 0, sizeof gPresentLines);
      run(0, 5.0, 165, bound165, 0, 0); gClock += 60000000000ull; run(0, 10.2, 165, bound165, 0, 0);
      printf("after an idle minute: %s\n", gLines.empty() ? "(none)" : gLines[0].c_str());
      assert(gLines.size() == 1 && gLines[0].find("(165/s, panel 165 Hz)") != std::string::npos && "an idle pause must not read as starvation"); }
    // A desktop that presents only 15 frames a second with no misses (upstream starvation) is reported too.
    { gLines.clear(); gClock = 1000; memset(gPresentWin, 0, sizeof gPresentWin); memset(gPresentLines, 0, sizeof gPresentLines);
      run(0, 10.2, 15, bound165, 0, 0);
      printf("15/s presents: %s\n", gLines.empty() ? "(none)" : gLines[0].c_str());
      assert(gLines.size() == 1 && gLines[0].find("(15/s, panel 165 Hz)") != std::string::npos); }
    // Two heads keep their own line budgets: the busy one running out does not silence the other.
    { gLines.clear(); gClock = 1000; memset(gPresentWin, 0, sizeof gPresentWin); memset(gPresentLines, 0, sizeof gPresentLines);
      for (int w = 0; w < 60; w++) run(0, 10.2, 100, bound165, 0, 0);
      gLines.clear(); run(1, 10.2, 100, bound165, 0, 0);
      assert(gLines.size() == 1 && "a second head's first window prints after the first head spent its budget"); }
    // Heads keep their own windows.
    { gLines.clear(); gClock = 1000; memset(gPresentWin, 0, sizeof gPresentWin); memset(gPresentLines, 0, sizeof gPresentLines);
      assert(!presentStatsNote(9, bound165, true, true, true, 1) && "a head past the table is ignored"); }
    // The count of lines is capped: 40 windows print, then every 10th.
    { gLines.clear(); gClock = 1000; memset(gPresentWin, 0, sizeof gPresentWin); memset(gPresentLines, 0, sizeof gPresentLines);
      for (int w = 0; w < 100; w++) run(0, 10.2, 100, bound165, 0, 0);
      printf("100 busy windows: %zu line(s)\n", gLines.size());
      assert(gLines.size() >= 40 && gLines.size() <= 47 && "the log must not take a line for every window of a long session"); }
    puts("ok");
}
'''


def run(h):
    with tempfile.TemporaryDirectory() as d:
        src = pathlib.Path(d) / 't.cpp'
        src.write_text(fixture.replace('HELPER', h))
        exe = pathlib.Path(d) / 't'
        c = subprocess.run(['clang++', '-std=c++17', '-O1', '-o', str(exe), str(src)], capture_output=True, text=True)
        assert c.returncode == 0, c.stderr
        r = subprocess.run([str(exe)], capture_output=True, text=True)
        return r


good = run(helper)
print(good.stdout.strip())
assert good.returncode == 0, 'the shipped helper fails its own arms:\n' + good.stdout + good.stderr
for name, ctl in (('idle windows printed', idle_control), ('window never restarts', nowindow_control)):
    r = run(ctl)
    assert r.returncode != 0, 'negative control did not fail: ' + name
    print('control fires (%s)' % name)
print('present stats: all arms pass')
