#!/usr/bin/env python3
# vendor_hooks.py <plugin.json> <Metal-<r>.json> <Metal-<r>.s> <vendor bundle binary>:
# the methods a real vendor Metal driver of that release implements that are outside every Apple protocol, not in
# Apple's base class, sent by that release's Metal, and absent from our counterpart class: the subclass hooks we lack.
import sys, json, re, subprocess, os, tempfile
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
P = json.load(open(sys.argv[1]))["classes"]; M = json.load(open(sys.argv[2])); S = sys.argv[3]; B = sys.argv[4]
sent = set()
for l in open(S, errors="replace"):
    m = re.search(r"Objc selector ref: (\S+)", l) or re.search(r"Objc message: [-+]\[\S+ ([^\]]+)\]", l)
    if m: sent.add(m.group(1))
ov = tempfile.NamedTemporaryFile(suffix=".ov", delete=False); ov.close(); js = ov.name + ".json"
open(ov.name, "w").write(subprocess.run(["otool", "-arch", "x86_64", "-ov", B], capture_output=True, text=True, errors="replace").stdout)
subprocess.run([sys.executable, os.path.join(os.path.dirname(os.path.abspath(__file__)), "objcov.py"), ov.name, js], capture_output=True)
A = json.load(open(js))["classes"]; os.unlink(ov.name); os.unlink(js)
def allm(C, n):
    o = set()
    while n in C: o |= set(C[n]["imeth"]); n = C[n]["super"]
    return o
def apple_root(n):
    seen = set()
    while n in A and n not in seen: seen.add(n); n = A[n]["super"]
    return n
cp = {"MTLIOAccelDevice": "NVMTLDevice", "MTLIOAccelCommandQueue": "NVMTLCommandQueue", "MTLIOAccelCommandBuffer": "NVMTLCommandBuffer",
      "MTLIOAccelBuffer": "NVMTLBuffer", "MTLIOAccelTexture": "NVMTLTexture", "MTLIOAccelRenderCommandEncoder": "NVMTLRenderCommandEncoder",
      "MTLIOAccelBlitCommandEncoder": "NVMTLBlitCommandEncoder", "MTLIOAccelComputeCommandEncoder": "NVMTLComputeCommandEncoder",
      "MTLIOAccelHeap": "NVMTLHeap", "MTLIOAccelIndirectCommandBuffer": "NVMTLIndirectCommandBuffer",
      "MTLIOAccelAccelerationStructure": "NVMTLAccelerationStructure", "MTLIOAccelParallelRenderCommandEncoder": "NVMTLParallelRenderCommandEncoder",
      "MTLIOAccelDebugCommandEncoder": None, "MTLIOAccelResource": None}
allproto = set()
for p in M["protocols"].values(): allproto |= set(p["imeth"]) | set(p["oimeth"])
print("%s: %d classes, Metal sends %d distinct selectors" % (os.path.basename(B), len(A), len(sent)))
tot = 0; pairs = 0
for k, v in sorted(A.items()):
    s = apple_root(k)
    if s not in cp or cp[s] is None: continue
    pairs += 1
    mine = allm(P, cp[s]); base = allm(M["classes"], s); vend = allm(A, k)
    cand = sorted(x for x in vend - allproto - base if x in sent and not x.startswith("."))
    hooks = [x for x in cand if x not in mine]
    tot += len(hooks); ncand = globals().get("ncand", 0) + len(cand); globals()["ncand"] = ncand
    if os.environ.get("BRIEF"):
        if cand: print("  %-34s : %-30s candidates %d (%s) we lack %d" % (k, s, len(cand), " ".join(cand)[:160], len(hooks)))
        continue
    print("  %-34s : %-36s vendor methods %3d, hooks we lack %d %s" % (k, s, len(vend), len(hooks), " ".join(hooks)))
print("  class pairs %d, hook candidates (vendor method, outside protocols and base, sent by Metal) %d, of those we lack: %d" % (pairs, globals().get("ncand", 0), tot))
