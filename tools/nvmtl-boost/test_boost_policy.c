/* test_boost_policy: the run-time boost switch's decisions. cc -I. test_boost_policy.c -o /tmp/tbp && /tmp/tbp */
#include <stdio.h>
#include "boost_policy.h"
static int fails;
static void arm(const char *n, int ok) { printf("%s  %s\n", ok ? "ok  " : "FAIL", n); fails += !ok; }
static enum boost_setting P(const char *s) { return boost_parse(s, strlen(s)); }
int main(void)
{
    arm("an empty file leaves the setting unset", P("") == BOOST_UNSET);
    arm("gpuboost=1 turns it on", P("gpuboost=1\n") == BOOST_ON);
    arm("gpuboost=0 turns it off", P("gpuboost=0\n") == BOOST_OFF);
    arm("the last line wins", P("gpuboost=1\ngpuboost=0\n") == BOOST_OFF && P("gpuboost=0\ngpuboost=1") == BOOST_ON);
    arm("other keys are ignored", P("fan=3\nname=gpuboost=1\n") == BOOST_UNSET);
    arm("a garbled value changes nothing (it does not undo an earlier line)", P("gpuboost=1\ngpuboost=yes\n") == BOOST_ON && P("gpuboost=2\n") == BOOST_UNSET);
    arm("a line with trailing blanks and CRLF still parses", P("gpuboost=1 \r\n") == BOOST_ON);
    arm("leading blanks are allowed", P("  gpuboost=0\n") == BOOST_OFF);
    arm("a file without a final newline parses", P("gpuboost=1") == BOOST_ON);
    arm("on and not held: take the request", boost_decide(BOOST_ON, 0) == BOOST_HOLD);
    arm("on and held: nothing to do", boost_decide(BOOST_ON, 1) == BOOST_KEEP);
    arm("off and held: clear the request", boost_decide(BOOST_OFF, 1) == BOOST_RELEASE);
    arm("off and not held: nothing to do", boost_decide(BOOST_OFF, 0) == BOOST_KEEP);
    arm("unset never starts or stops a boost", boost_decide(BOOST_UNSET, 0) == BOOST_KEEP && boost_decide(BOOST_UNSET, 1) == BOOST_KEEP);
    arm("only a root-owned file that others cannot write is trusted", boost_file_trusted(0, 0644) && !boost_file_trusted(501, 0644) && !boost_file_trusted(0, 0666) && !boost_file_trusted(0, 0620));
    printf(fails ? "FAILED %d\n" : "ok\n", fails);
    return fails != 0;
}
