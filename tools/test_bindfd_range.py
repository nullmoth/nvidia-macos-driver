"""NVRM's bind-fd accepts every value the userland driver sends and refuses the rest.

The userland driver binds a connection to its fd number (0..), and detaches one it pools or keeps alive by binding
it to -2, sign-extended to 64 bits. A range check that refused the detach value left two connections answering for
one fd, and WindowServer's Vulkan device could not be created. The check is compiled here from the kext's own source
line and run as C."""
import os, re, subprocess, tempfile, unittest

ROOT = os.path.join(os.path.dirname(os.path.abspath(__file__)), "..")
SENT = {"fd 0": 0, "fd 5": 5, "fd 4095": 4095, "detach -2": (1 << 64) - 2, "detach -1": (1 << 64) - 1}
REFUSED = {"2^31": 1 << 31, "2^32-1": (1 << 32) - 1, "2^32-2": (1 << 32) - 2, "huge": 1 << 40,
           "below int32": (1 << 64) - (1 << 31) - 1}


def accepts(condition, decl):
    src = r'''#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
static int ok(uint64_t raw) { struct { uint64_t scalarInput[1]; } a = {{raw}}, *args = &a; %s
    if (%s) return 0; return 1; }
int main(int c, char **v) { for (int i = 1; i < c; i++) printf("%%d\n", ok(strtoull(v[i], 0, 0))); return 0; }
''' % (decl, condition)
    with tempfile.TemporaryDirectory() as t:
        s, e = os.path.join(t, "b.c"), os.path.join(t, "b")
        open(s, "w").write(src)
        subprocess.run(["xcrun", "clang", "-w", "-o", e, s], check=True)
        vals = list(SENT.values()) + list(REFUSED.values())
        out = subprocess.run([e] + [str(x) for x in vals], capture_output=True, text=True, check=True).stdout.split()
    return dict(zip(list(SENT) + list(REFUSED), [x == "1" for x in out]))


class BindFdRange(unittest.TestCase):
    def test_every_value_the_driver_sends_is_accepted_and_the_rest_refused(self):
        src = open(os.path.join(ROOT, "kexts/NVRM/NVRM.cpp")).read()
        decl = re.search(r"const int64_t v = \(int64_t\)args->scalarInput\[0\];", src).group(0)
        cond = re.search(r"if \((v > 0x7FFFFFFFLL \|\| v < -0x80000000LL)\) return kIOReturnBadArgument;", src).group(1)
        got = accepts(cond, decl)
        self.assertEqual({k: got[k] for k in SENT}, {k: True for k in SENT})
        self.assertEqual({k: got[k] for k in REFUSED}, {k: False for k in REFUSED})

    def test_negative_control_the_first_patch_refused_the_detach_value(self):
        got = accepts("args->scalarInput[0] > 0x7FFFFFFFULL", "")
        self.assertFalse(got["detach -2"], "the old check should refuse -2; otherwise this test proves nothing")


if __name__ == "__main__":
    unittest.main()
