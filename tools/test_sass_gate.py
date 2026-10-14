"""Compiled kernels run only on GeForce RTX 50 series GPUs (sm_120): the name gate in plugin/nvmtl_sass_gate.h.
Built and run as C on this machine; no GPU needed."""
import os, subprocess, tempfile, unittest

HERE = os.path.dirname(os.path.abspath(__file__))
GATE = os.path.join(HERE, "..", "plugin", "nvmtl_sass_gate.h")

ACCEPT = ["NVIDIA GeForce RTX 5060", "NVIDIA GeForce RTX 5070 Laptop GPU", "NVIDIA GeForce RTX 5090",
          "NVIDIA GeForce RTX 5090 D", "NVIDIA GeForce RTX 5050", "NVIDIA GeForce RTX 5060 Ti"]
REFUSE = ["NVIDIA RTX 5000 Ada Generation", "NVIDIA GeForce RTX 4090", "NVIDIA GeForce RTX 3050 Laptop GPU",
          "NVIDIA GeForce RTX 2060 SUPER", "NVIDIA GeForce GTX 1650", "NVIDIA GeForce RTX 50",
          "NVIDIA GeForce RTX 50601", "NVIDIA GeForce RTX 505", "", "NV194"]

HARNESS = r'''
#include <stdio.h>
#include "%s"
%s
int main(int argc, char **argv) {
    for (int i = 1; i < argc; i++) printf("%%d\n", GATE(argv[i]));
    printf("%%d\n", GATE(NULL));
    return 0;
}
'''


def run(gate_expr, extra=""):
    with tempfile.TemporaryDirectory() as t:
        src, exe = os.path.join(t, "h.c"), os.path.join(t, "h")
        with open(src, "w") as f:
            f.write(HARNESS % (os.path.abspath(GATE), "#define GATE(n) (%s)\n%s" % (gate_expr, extra)))
        subprocess.run(["xcrun", "clang", "-Wall", "-Werror", "-o", exe, src], check=True)
        out = subprocess.run([exe] + ACCEPT + REFUSE, check=True, capture_output=True, text=True).stdout.split()
    return [int(x) for x in out]


class SassGate(unittest.TestCase):
    def test_every_rtx_50_name_is_accepted_and_every_other_card_refused(self):
        got = run("nvmtl_sass_device_name_ok(n)")
        self.assertEqual(got[:len(ACCEPT)], [1] * len(ACCEPT), dict(zip(ACCEPT, got)))
        self.assertEqual(got[len(ACCEPT):-1], [0] * len(REFUSE), dict(zip(REFUSE, got[len(ACCEPT):])))
        self.assertEqual(got[-1], 0, "a missing name must refuse")

    def test_negative_control_a_substring_gate_lets_the_ada_workstation_card_through(self):
        naive = "static int naive(const char *n) { return n && strstr(n, \"RTX 50\") != NULL; }"
        got = run("naive(n)", naive)
        ada = len(ACCEPT) + REFUSE.index("NVIDIA RTX 5000 Ada Generation")
        self.assertEqual(got[ada], 1, "the naive gate should accept RTX 5000 Ada; the test would prove nothing")


if __name__ == "__main__":
    unittest.main()
