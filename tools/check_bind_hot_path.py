#!/usr/bin/env python3
"""check_bind_hot_path.py - the per-draw bind methods of the render encoder must not build strings or boxed numbers.

Apple's OpenGL-on-Metal layer rebinds every texture and sampler before each draw, so these methods run thousands of
times a frame. One profile showed an NSString format in setFragmentTexture costing 13.5 us per draw. The check reads the
body of each method in plugin/NVMTLObjects.m and refuses a call that allocates a string, a number or a collection on the
common path (a once-per-process note behind a guard is not on the common path and is written with integers).

usage: check_bind_hot_path.py [--selftest] [path to NVMTLObjects.m]"""
import re, sys

METHODS = ["setFragmentTexture:(id<MTLTexture>)t atIndex", "setVertexTexture:(id<MTLTexture>)t atIndex",
           "setFragmentSamplerState:(id<MTLSamplerState>)s atIndex", "setVertexSamplerState:(id<MTLSamplerState>)s atIndex",
           "setFragmentBuffer:(id<MTLBuffer>)b offset", "setVertexBuffer:(id<MTLBuffer>)b offset"]
BAD = re.compile(r"stringWithFormat|NSString\s+stringWith|\[NSNumber|@\(|\[NSMutable(Set|Array|Dictionary) new\]|\[\[NSMutable|dictionaryWith|arrayWith")
# lazily created containers kept in an ivar (if (!x) x = [NSMutable... new]) run once per encoder, not per bind
LAZY = re.compile(r"if\s*\(\s*!\s*[_\w>-]+\s*\)\s*[_\w>-]+\s*=\s*\[\s*NSMutable\w+\s+new\s*\]")

def body(src, sig):
    i = src.find("- (void)" + sig)
    if i < 0: return None
    j = src.index("{", i); depth = 0
    for k in range(j, len(src)):
        depth += (src[k] == "{") - (src[k] == "}")
        if depth == 0: return src[j:k + 1]
    return None

def problems(src):
    out = []
    for sig in METHODS:
        b = body(src, sig)
        if b is None: out.append("%s: method not found" % sig.split(":")[0]); continue
        b = LAZY.sub("", b)
        for ln in b.splitlines():
            if BAD.search(ln): out.append("%s: %s" % (sig.split(":")[0], ln.strip()[:120]))
    return out

def selftest():
    bad = "- (void)setFragmentTexture:(id<MTLTexture>)t atIndex:(NSUInteger)i {\n NSString *k = [NSString stringWithFormat:@\"%lu\", i];\n}\n"
    good = "- (void)setFragmentTexture:(id<MTLTexture>)t atIndex:(NSUInteger)i {\n uint64_t k = i;\n if (!_x) _x = [NSMutableSet new];\n}\n"
    ok = True
    for name, src, want in (("a per-bind NSString format is refused", bad, True), ("integers and a lazily made ivar container pass", good, False)):
        got = any("setFragmentTexture" in p and "not found" not in p for p in problems(src))
        print(("ok   " if got == want else "FAIL ") + name); ok &= got == want
    return 0 if ok else 1

if __name__ == "__main__":
    if "--selftest" in sys.argv: sys.exit(selftest())
    path = [a for a in sys.argv[1:] if not a.startswith("-")]
    src = open(path[0] if path else "plugin/NVMTLObjects.m").read()
    p = problems(src)
    for x in p: print("FAIL", x)
    print("bind hot path: %s" % ("clean" if not p else "%d allocation(s) on the per-bind path" % len(p)))
    sys.exit(1 if p else 0)
