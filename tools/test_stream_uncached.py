#!/usr/bin/env python3
"""The command and upload stream must not be cached by the GPU.

NVK appends every small queue push and every upload to one chunk, each right behind the last (4-byte alignment), so a
new push usually starts in a cache line the GPU has already read. The GPU's cache does not see CPU writes; coherence
comes from the invalidate that ends each submission. The host fetches pushbuffer words ahead of the engine, so it can
fetch the next push before the engine has run the previous submission's invalidate. With a cached chunk it then reads
the shared line as it was before the new words were written. Measured on a login: the host reported an invalid
pushbuffer entry inside a 10-word push whose words were correct in memory, and the push's third word stood in the
host's saved state where a header would - as if the first two (same line as the previous push) had been read as
they were before the write. With stream chunks uncached the fault did not return in 6 logins of 6 (4 of 4 before).
nvk/nvk-stream-uncached.patch stops caching stream chunks. The model below has the cache's real rules (a line is
filled on first read, CPU writes do not reach it, the invalidate empties it) and the order the host uses."""
import pathlib, sys

R = pathlib.Path(__file__).resolve().parents[1]
patch = (R / "nvk/nvk-stream-uncached.patch").read_text()
bld = (R / "build/build_nvk.sh").read_text()
inputs = (R / "build/build_release_inputs.sh").read_text()
fails = 0
LINE = 64


def arm(name, ok, detail=""):
    global fails
    print(("ok   " if ok else "FAIL ") + name + (f" ({detail})" if detail and not ok else ""))
    fails += not ok


class Gpu:
    def __init__(self, ram, cached):
        self.ram, self.cached, self.lines = ram, cached, {}

    def fetch(self, off, n):                      # the host reading n words of a push
        out = []
        for a in range(off, off + 4 * n, 4):
            ln = a // LINE
            if self.cached:
                if ln not in self.lines:
                    self.lines[ln] = bytes(self.ram[ln * LINE:(ln + 1) * LINE])
                src, base = self.lines[ln], ln * LINE
            else:
                src, base = self.ram, 0
            out.append(int.from_bytes(src[a - base:a - base + 4], "little"))
        return out

    def invalidate(self):                         # the engine running the tail of a submission
        self.lines.clear()


def put(ram, off, words):
    for i, w in enumerate(words):
        ram[off + 4 * i:off + 4 * i + 4] = w.to_bytes(4, "little")


FIRST = [0x2003255d, 2, 0x00200000, 0x3ff, 0x80002091, 0x2003055d, 2, 0x00200000, 0x3ff, 0x8000050a]
SECOND = [0x20032557, 2, 0x00110000, 0xfff, 0x80002509, 0x20030557, 2, 0x00110000, 0xfff, 0x80000509]
A, B = 0x4050, 0x4078                              # the measured offsets: the second push starts in the first one's line


def run(cached, fetch_before_invalidate):
    ram = bytearray(0x8000); g = Gpu(ram, cached)
    put(ram, A, FIRST); g.fetch(A, len(FIRST))     # first push written, submitted and fetched
    put(ram, B, SECOND)                            # second push written behind it
    if not fetch_before_invalidate:
        g.invalidate()
    return g.fetch(B, len(SECOND))


stale = run(True, True)
arm("control: a cached chunk, next push fetched before the invalidate, loses the words that share the old line",
    stale[:2] == [0, 0] and stale[2:] == SECOND[2:], hex(stale[0]))
arm("control: the first word the host then sees is the push's third word, the value its saved state held",
    next(w for w in stale if w) == 0x00110000)
arm("a cached chunk is only right when the invalidate happens to run first", run(True, False) == SECOND)
arm("an uncached chunk reads the new push whole in the order the host uses", run(False, True) == SECOND)
arm("an uncached chunk reads it whole in the other order too", run(False, False) == SECOND)
arm("the two pushes really share a line at the measured offsets", A // LINE == B // LINE)

plus = [l[1:].strip() for l in patch.splitlines() if l.startswith("+") and not l.startswith("+++")]
minus = [l[1:].strip() for l in patch.splitlines() if l.startswith("-") and not l.startswith("---")]
arm("the patch touches only the stream allocator", patch.count("diff -ruN") == 1 and "nvk_mem_stream.c" in patch)
arm("stream chunks are plain system memory, never the cached kind",
    "return NVKMD_MEM_GART;" in plus and not any("NVK69_MEM_GPU_CACHED" in l for l in plus))
arm("the cached return is the line removed", any("NVK69_MEM_GPU_CACHED" in l and "return" in l for l in minus))
c30 = next((l for l in bld.splitlines() if l.strip().startswith("CHAIN=") and "nvk-stream-uncached" in l), "")
c29 = [l for l in bld.splitlines() if l.strip().startswith("CHAIN=") and "nvk-diag-nofollow\" ;;" in l]
arm("the 3.0 set applies it last", c30.rstrip().endswith('nvk-stream-uncached" ;;'))
arm("the 2.9 set is unchanged, so 2.9 still rebuilds as shipped", len(c29) == 1)
arm("the release build uses the 3.0 set", '"$OUT/nvk-build" 3.0 ||' in inputs)
print("FAILED" if fails else "all ok")
sys.exit(1 if fails else 0)
