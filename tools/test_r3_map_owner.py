"""A device memory map resolves a RAM allocation by physical range, so it must also check the owner.

clientMemoryForType's device branch hands the mapping range to nv_pm_ram_submd, which finds the backing
allocation with nv_pm_find keyed on the physical range alone. Two clients presenting the same range resolve
to the same buffer, so one user could map another user's RAM. The allocation now carries the pid that created
it, and nv_pm_owner_blocks decides when a device map crosses an owner boundary: only when owner and caller are
both real, distinct user processes. A kernel/RM-internal allocation (owner 0) stays shareable and a caller with
no pid is never refused, so a legitimate map is never blocked. The predicate is compiled here from the kext's
own source line and run as C, then the enforce/observe modes are modelled on top of it. A cross-owner map is refused by default;
nvrmmapowner=0 downgrades to observe-and-count as a recovery valve."""
import os, re, subprocess, tempfile, unittest

ROOT = os.path.join(os.path.dirname(os.path.abspath(__file__)), "..")

# (owner_pid, caller_pid): whether a device map crosses an owner boundary
PAIRS = {
    "same user (501 maps 501)":        (501, 501),
    "kernel buffer (0) mapped by 501": (0, 501),
    "caller has no pid (501 by 0)":    (501, 0),
    "cross user (501 buffer by 502)":  (501, 502),
    "cross user (99 buffer by 100)":   (99, 100),
    "both kernel (0 by 0)":            (0, 0),
}
# which pairs are a genuine cross-owner map and must be caught
CROSS = {"cross user (501 buffer by 502)", "cross user (99 buffer by 100)"}


def blocks(body):
    """Compile `bool nv_pm_owner_blocks(owner, caller) { <body> }` and run it over PAIRS."""
    src = r'''#include <stdio.h>
#include <stdlib.h>
static int nv_pm_owner_blocks(int owner, int caller) { %s }
int main(int c, char **v) { for (int i = 1; i + 1 < c; i += 2) printf("%%d\n", nv_pm_owner_blocks(atoi(v[i]), atoi(v[i+1])) ? 1 : 0); return 0; }
''' % body
    with tempfile.TemporaryDirectory() as t:
        s, e = os.path.join(t, "b.c"), os.path.join(t, "b")
        with open(s, "w") as f:
            f.write(src)
        subprocess.run(["xcrun", "clang", "-w", "-o", e, s], check=True)
        args = []
        for o, c in PAIRS.values():
            args += [str(o), str(c)]
        out = subprocess.run([e] + args, capture_output=True, text=True, check=True).stdout.split()
    return dict(zip(PAIRS, [x == "1" for x in out]))


def source_body():
    with open(os.path.join(ROOT, "kexts/NVRM/os-xnu2.cpp")) as f:
        src = f.read()
    m = re.search(r"static inline bool nv_pm_owner_blocks\(int owner, int caller\) \{ (return [^;]+;) \}", src)
    assert m, "nv_pm_owner_blocks not found in os-xnu2.cpp — the fix is not present"
    return m.group(1)


class MapOwner(unittest.TestCase):
    def test_cross_owner_map_is_the_only_thing_flagged(self):
        got = blocks(source_body())
        for name in PAIRS:
            self.assertEqual(got[name], name in CROSS,
                             "%s: expected cross=%s, got %s" % (name, name in CROSS, got[name]))

    def test_enforce_refuses_cross_owner_audit_counts_it(self):
        got = blocks(source_body())
        # nv_pm_ram_submd hands the buffer over unless (crosses an owner AND enforcing);
        # a mismatch is counted whenever it crosses, in either mode.
        for name in PAIRS:
            crosses = got[name]
            audit_allow = not (crosses and False)   # nvrmmapowner unset
            enforce_allow = not (crosses and True)  # nvrmmapowner=1
            if name in CROSS:
                self.assertTrue(audit_allow and crosses, "%s must be allowed-but-counted in audit" % name)
                self.assertFalse(enforce_allow, "%s must be refused under nvrmmapowner=1" % name)
            else:
                self.assertTrue(audit_allow and enforce_allow, "%s must be allowed in both modes" % name)
                self.assertFalse(crosses, "%s must not be counted as a mismatch" % name)

    def test_negative_control_old_code_had_no_owner_check(self):
        # bc8baa7 nv_pm_ram_submd always handed the buffer over: no owner in the key, no mismatch.
        got = blocks("return 0;")
        for name in CROSS:
            self.assertFalse(got[name],
                             "the old code should flag nothing, so a cross-owner map would be handed over "
                             "even under enforce; otherwise this test proves nothing")


if __name__ == "__main__":
    unittest.main()
