from pathlib import Path
import os
import shlex
import shutil
import subprocess
import tempfile


ROOT = Path(__file__).resolve().parents[1]
SOURCE = ROOT / "kexts/NMHDA/nmhda.cpp"

HARNESS = r'''
#include <cassert>
#include <cstdint>
#include <cstddef>
#include <cstring>
using IOReturn = unsigned;
using mach_vm_address_t = uintptr_t;
using NMHDAExecuteVerb = IOReturn (*)(void *, uint16_t, uint16_t, uint16_t, unsigned *, bool);
static NMHDAExecuteVerb gTopologyVerb;
static IOReturn verb(void *, uint16_t, uint16_t, uint16_t, unsigned *output, bool) {
    *output = 0x10de00ab;
    return 0;
}
struct KernelPatcher {
    bool error = false;
    uintptr_t solveSymbol(size_t index, const char *symbol, uintptr_t, size_t) {
        assert(!std::strcmp(symbol, "__ZN16IOHDACodecDevice11executeVerbEtttPjb"));
        error = index != 7;
        return index == 7 ? reinterpret_cast<uintptr_t>(verb) : 0;
    }
    void clearError() { error = false; }
};
static struct { size_t loadIndex = 111; } topologyFamily;
static void report(const char *, const char *, ...) {}
'''

CHECKS = r'''
int main() {
    KernelPatcher patcher;
    topologyFamilyLoaded(nullptr, patcher, 2, 0, 0);
    assert(!gTopologyVerb && !patcher.error);
    // Bind the real image even when its runtime index differs from the registration.
    topologyFamilyLoaded(nullptr, patcher, 7, 0, 0);
    assert(gTopologyVerb == verb && !patcher.error);
    topologyFamilyLoaded(nullptr, patcher, 8, 0, 0);
    assert(gTopologyVerb == verb && "unrelated image erased the resolved GET entry");
    topologyFamilyLoaded(nullptr, patcher, 7, 0, 0);
    unsigned output = 0;
    assert(gTopologyVerb(nullptr, 0, 0xf00, 0, &output, true) == 0);
    assert(output == 0x10de00ab);
}
'''


def main():
    source = SOURCE.read_text(encoding="utf-8")
    start = source.index("static void topologyFamilyLoaded(")
    callback = source[start:source.index("static void pluginStart()", start)]
    command = os.environ.get("CXX")
    compiler = shlex.split(command) if command else None
    if not compiler:
        path = shutil.which("clang++") or shutil.which("g++") or shutil.which("c++")
        if not path:
            raise SystemExit("NMHDA callback test requires clang++ or g++ on PATH")
        compiler = [path]
    with tempfile.TemporaryDirectory(prefix="nmhda-topology-") as temp:
        cpp = Path(temp) / "test.cpp"
        exe = Path(temp) / ("test.exe" if os.name == "nt" else "test")
        cpp.write_text(HARNESS + callback + CHECKS, encoding="utf-8")
        subprocess.run(compiler + ["-std=c++17", "-Wall", "-Wextra", "-Werror",
                                  str(cpp), "-o", str(exe)], check=True)
        subprocess.run([str(exe)], check=True)
    print("PASS: topology GET survives unrelated and repeated image callbacks")


if __name__ == "__main__":
    main()
