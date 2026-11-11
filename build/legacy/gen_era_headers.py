#!/usr/bin/env python3
# gen_era_headers.py <named family tables json> <kernel+graphics-family tables json> <accel source dir> <out dir> <era>
# Writes iofam and re headers for an older release: every class body is regenerated from that release's own virtual
# table (slot order, names, which slots are pure) and the instance size its metaclass registers. Declaration text
# (return types, argument spelling) is taken from the hand-written macOS 15 headers where the same method exists.
import json, os, re, subprocess, sys
named = json.load(open(sys.argv[1])); kern = json.load(open(sys.argv[2])); src, out, era = sys.argv[3], sys.argv[4], sys.argv[5]
famsyms = [l.strip() for l in open(sys.argv[6])] if len(sys.argv) > 6 else []   # symbols this release's family defines (nm -gU)
dropped_nv = []
# Return types that only an inline body reveals (often in a subclass) are written to this file by one run and read by
# the next, so the base's declaration and the override agree: the generator is run twice per release.
revfile = sys.argv[8] if len(sys.argv) > 8 else None
revealed = {tuple(k): v for k, v in json.load(open(revfile))} if revfile and os.path.exists(revfile) else {}
# Methods the family defines INLINE in its own headers are not exported: every driver built against Apple's headers
# carries its own copy. Those copies are read from the family binary (argument 7) and reproduced when they are one of
# a few trivial shapes (a field read, a constant, nothing); anything else stays a declaration and shows up unresolved.
fambin = sys.argv[7] if len(sys.argv) > 7 else None
_dis = None
def inline_body(mangled, ret):
    global _dis
    if not fambin or mangled in famsyms: return None, ret
    if _dis is None:
        _dis = {}; cur = None
        for l in subprocess.run(["otool", "-arch", "x86_64", "-tV", fambin], capture_output=True, text=True, errors="replace").stdout.splitlines():
            if l.endswith(":") and not l.startswith("\t") and not l[0].isdigit():
                cur = l[:-1]
                if cur in _dis: cur = None          # keep the first copy
                else: _dis[cur] = []
                continue
            if cur is not None and "\t" in l: _dis[cur].append(l.split("\t", 1)[1].strip())
    ins = [x for x in _dis.get(mangled, []) if not x.startswith("nop")]
    if not ins: return None, ret
    # Forwarders that end in a jump instead of a return. @ARGS@ / @CALL@ are filled in by the caller with the
    # method's own parameters; a jump passes every argument register through, so forwarding all of them is the same call.
    fw = [x for x in ins if not re.match(r"(pushq\s+%rbp|movq\s+%rsp, %rbp|popq\s+%rbp|movzbl\s+%(dl|sil|cl), %e(dx|si|cx))$", x)]
    r2 = ret        # the declared type stays: classes of ours override some of these
    def slot(rest):
        if len(rest) == 1:
            m = re.match(r"jmpq\s+\*0x([0-9a-f]+)\(%rax\)$", rest[0]); return m and int(m.group(1), 16)
        if len(rest) == 2 and re.match(r"jmpq\s+\*%rax$", rest[1]):
            m = re.match(r"movq\s+0x([0-9a-f]+)\(%rax\), %rax$", rest[0]); return m and int(m.group(1), 16)
        return None
    if len(fw) >= 2 and re.match(r"movq\s+\(%rdi\), %rax$", fw[0]):
        off = slot(fw[1:])
        if off is not None and off % 8 == 0:      # this object's own virtual at that table offset
            return "{ typedef %s (*F_)(void *@ARGS@); return ((F_)(*(void ***)this)[%d])((void *)this@CALL@); }" % (r2, off // 8), r2
    if len(fw) >= 3 and re.match(r"movq\s+\(%rdi\), %rax$", fw[1]):
        m = re.match(r"movq\s+0x([0-9a-f]+)\(%rdi\), %rdi$", fw[0]); off = slot(fw[2:])
        if m and off is not None and off % 8 == 0:  # a virtual of the object this one holds at that field
            return "{ void *o_ = *(void **)((unsigned char *)this + 0x%s); typedef %s (*F_)(void *@ARGS@); return ((F_)(*(void ***)o_)[%d])(o_@CALL@); }" % (m.group(1), r2, off // 8), r2
    if len(fw) == 2 and fw[1] == "retq":
        m = re.match(r"mov([lqb])\s+%(esi|rsi|sil), 0x([0-9a-f]+)\(%rdi\)$", fw[0])
        if m:                                       # stores its first argument in a field
            ty = {"l": "unsigned int", "q": "unsigned long long", "b": "unsigned char"}[m.group(1)]
            tail = "" if ret == "void" else "return (%s)0; " % ret      # the family's body returns nothing meaningful
            return "{ *(%s *)((unsigned char *)this + 0x%s) = (%s)a0; %s@NAMED@}" % (ty, m.group(3), ty, tail), ret
    if "retq" not in ins[-2:]: return None, ret
    core = [x for x in ins if not re.match(r"(pushq\s+%rbp|movq\s+%rsp, %rbp|popq\s+%rbp|retq)$", x)]
    if len(core) == 0: return ("{ }" if ret == "void" else None), ret
    if len(core) == 1:
        m = re.match(r"movq\s+0x([0-9a-f]+)\(%rdi\), %rax$", core[0])
        if m:
            r2 = "void *" if ret == "void" else ret
            return "{ return *(%s *)((unsigned char *)this + 0x%s); }" % (r2, m.group(1)), r2
        m = re.match(r"movl\s+0x([0-9a-f]+)\(%rdi\), %eax$", core[0])
        if m:
            r2 = "unsigned int" if ret == "void" else ret
            return "{ return (%s)*(unsigned int *)((unsigned char *)this + 0x%s); }" % (r2, m.group(1)), r2
        if re.match(r"xorl\s+%eax, %eax$", core[0]): return "{ return 0; }", ("unsigned int" if ret == "void" else ret)
        m = re.match(r"mov[bl]\s+\$0x([0-9a-f]+), %(al|eax)$", core[0])
        if m: return "{ return 0x%s; }" % m.group(1), ("unsigned int" if ret == "void" else ret)
    return None, ret
# class on this release -> (base, [names the macOS 15 headers use for it], header file that carries it)
PLAN = [("IOAccelMemory", "OSObject", ["IOAccelMemory"], "iofam/IOAccelMemory.h"), ("IOAccelMemoryMap", "OSObject", ["IOAccelMemoryMap"], "iofam/IOAccelMemoryMap.h"),
        ("IOAccelResource2", "OSObject", ["IOAccelResource2"], "iofam/IOAccelResource2.h"), ("IOAccelShared2", "OSObject", ["IOAccelShared2"], "iofam/IOAccelShared2.h"),
        ("IOAccelTask", "OSObject", ["IOAccelTask"], "iofam/IOAccelTask.h"), ("IOAccelSysMemory", "IOAccelMemory", ["IOAccelSysMemory"], "iofam/IOAccelSysMemory.h"),
        ("IOAccelVidMemory", "IOAccelMemory", ["IOAccelVidMemory"], "iofam/IOAccelVidMemory.h"), ("IOAccelEventMachine2", "OSObject", ["IOAccelEventMachine2"], "iofam/IOAccelEventMachine2.h"),
        ("IOAccelEventMachineFast2", "IOAccelEventMachine2", ["IOAccelEventMachineFast2"], "iofam/IOAccelEventMachineFast2.h"),
        ("IOAccelSubmitter2", "IOUserClient", ["IOAccelSubmitter2"], "iofam/IOAccelSubmitter2.h"), ("IOAccelContext2", "IOAccelSubmitter2", ["IOAccelContext2"], "iofam/IOAccelContext2.h"),
        ("IOAccelGLContext2", "IOAccelContext2", ["IOAccelGLContext2"], "iofam/IOAccelGLContext2.h"), ("IOAccel2DContext2", "IOAccelContext2", ["IOAccel2DContext2"], "iofam/IOAccel2DContext2.h"),
        ("IOAccelCLContext2", "IOAccelContext2", ["IOAccelCLContext2"], "iofam/IOAccelCLContext2.h"), ("IOAccelVideoContext2", "IOAccelContext2", ["IOAccelVideoContext2"], "iofam/IOAccelVideoContext2.h"),
        ("IOAccelSurface2", "IOUserClient", ["IOAccelSurface", "IOAccelLegacySurface"], "iofam/IOAccelSurface.h"),
        ("IOAccelDisplayPipe2", "IOService", ["IOAccelDisplayPipe", "IOAccelLegacyDisplayPipe"], "iofam/IOAccelDisplayPipe.h"),
        ("IOAccelDisplayMachine2", "IOService", ["IOAccelDisplayMachine", "IOAccelLegacyDisplayMachine"], "iofam/IOAccelDisplayMachine.h"),
        ("IOAccelDevice2", "IOUserClient", ["IOAccelDevice2"], "re/IOAccelDevice2.h"), ("IOAccelSharedUserClient2", "IOUserClient", ["IOAccelSharedUserClient2"], "re/IOAccelSharedUserClient2.h"),
        ("IOGraphicsAccelerator2", "IOAccelerator", ["IOGraphicsAccelerator2"], "re/IOGraphicsAccelerator2.h")]
EMPTY = {"iofam/IOAccelLegacySurface.h": "IOAccelSurface.h", "iofam/IOAccelLegacyDisplayPipe.h": "IOAccelDisplayPipe.h", "iofam/IOAccelLegacyDisplayMachine.h": "IOAccelDisplayMachine.h"}
tables = dict(kern); tables.update({k: v for k, v in named.items()})
def mname(n):
    m = re.match(r"__ZNK?(\d+)", n)
    if not m: return n
    rest = n[m.end() + int(m.group(1)):]; k = re.match(r"(\d+)", rest)
    return rest[len(k.group(1)):len(k.group(1)) + int(k.group(1))] if k else rest
# a table read from a binary can run past its end (padding, or the next unnamed object): drop trailing empty slots
for t_ in tables.values():
    while t_["slots"] and t_["slots"][-1] in ("?0",):
        t_["slots"].pop()
        if "names" in t_: t_["names"].pop()
KERNEL = ("OSObject", "IORegistryEntry", "IOService", "IOUserClient", "IOAccelerator")
def nslots(c, child=None):
    """slot count of a base. For a kernel base read from the kernel binary the table end is not reliable, so it is
    the longest prefix of the child's table whose methods are the base's methods in the same order."""
    n = len(tables[c]["slots"])
    if c not in KERNEL or child is None: return n
    S, K = tables[child]["slots"], tables[c]["slots"]; i = 0
    while i < min(len(S), len(K)) and (mname(S[i]) == mname(K[i]) or S[i] == K[i]): i += 1
    return i
def norm(a): return re.sub(r"\s+", "", a)
def as_newest(text):
    """the hand-written headers carry per-release switches (NM_ERA); read them as the newest release sees them"""
    out = []; stack = []
    for l in text.split("\n"):
        t = l.strip()
        if t.startswith("#if") and "NM_ERA" in t:
            take = t.startswith("#if !defined(NM_ERA)")     # "not an era build, or era new enough" holds for the newest
            stack.append([take, True]); continue
        if stack and t.startswith("#else") and stack[-1][1]: stack[-1][0] = not stack[-1][0]; continue
        if stack and t.startswith("#endif") and stack[-1][1]: stack.pop(); continue
        if t.startswith("#if"): stack.append([stack[-1][0] if stack else True, False])
        elif t.startswith("#endif") and stack and not stack[-1][1]: stack.pop()
        if all(x[0] for x in stack): out.append(l)
    return "\n".join(out)
# declarations of the macOS 15 headers: (method, normalised args, const) -> return type
hand = {}
for d in ("iofam", "re"):
    for f in os.listdir(os.path.join(src, d)):
        for l in as_newest(open(os.path.join(src, d, f)).read()).split("\n"):
            m = re.match(r"\s*virtual\s+(.+?)\s*\b([A-Za-z_][A-Za-z0-9_]*)\((.*?)\)(\s*const)?\s*(= 0)?\s*(APPLE_KEXT_OVERRIDE)?\s*;", l)
            if m: hand.setdefault((m.group(2), norm(m.group(3)), bool(m.group(4))), m.group(1).strip())
def demangle(ms):
    syms = ["__ZN" + ("K" if x.startswith("K") else "") + "1X" + (x[1:] if x.startswith("K") else x) for x in ms]
    o = subprocess.run(["c++filt"], input="\n".join(syms), capture_output=True, text=True).stdout.splitlines()
    return dict(zip(ms, o))
allnames = sorted({x for c, *_ in PLAN for x in named[c]["names"] if x})
dem = demangle(allnames)
BUILTIN = set("unsigned int long short char bool void const signed float double volatile X".split())
fwd = set(); bodies = {}; report = []; emitted = {}; inlined = []
# Return types the newer headers cannot supply, read from Apple's vendor drivers of the release (what the method
# leaves in the return register and how the family's callers use it).
RET_KNOWN = {("canTmpAlloc", "unsignedint", False): "bool"}   # Apple's NVIDIA surface returns the constant 1; callers store and test %al
RENAMED = [("IOAccelSurface2", "IOAccelSurface"), ("IOAccelDisplayPipe2", "IOAccelDisplayPipe"), ("IOAccelDisplayMachine2", "IOAccelDisplayMachine")]
def old_spelling(a):
    for n_, o_ in RENAMED: a = re.sub(r"\b%s\b" % n_, o_, a)
    return a
for c, base, olds, hdr in PLAN:
    t = named[c]; start = nslots(base, c); S, N, P = t["slots"], t["names"], set(t["pure"]); lines = []; synth = 0; unnamed = 0
    cm = f"{len(c)}{c}"
    def decl(i, override):
        global synth, unnamed
        x = N[i]
        if not x:
            unnamed += 1; return f"    virtual void _pure{i}() = 0;"
        d = dem[x]; m = re.match(r"X::([A-Za-z_][A-Za-z0-9_]*)\((.*)\)( const)?$", d)
        if not m: return f"    virtual void _slot{i}();   // {d}"
        name, args, const = m.group(1), m.group(2), bool(m.group(3))
        args = re.sub(r"\bX\b", c, args)
        # a type read from this release's own code outranks one copied from the newer headers
        ret = revealed.get((name, norm(args), const)) or RET_KNOWN.get((name, norm(args), const)) or hand.get((name, norm(args), const)) or hand.get((name, norm(old_spelling(args)), const))
        if ret is None:
            # the same method under the names the newer headers use for this class
            for o_ in olds:
                ret = hand.get((name, norm(args.replace(c, o_)), const))
                if ret: break
        if ret is None: ret = "void"; synth += 1
        for tok in re.findall(r"[A-Za-z_][A-Za-z0-9_]*", args):
            if tok not in BUILTIN: fwd.add(tok)
        body = None
        if i not in P and (S[i].startswith("__ZN" + cm) or S[i].startswith("__ZNK" + cm)):
            ret0 = ret; body, ret = inline_body(S[i], ret)
            if body and ret != ret0: revealed[(name, norm(args), const)] = ret
            if body: inlined.append(c + "::" + name)
        if body and "@" in body:
            # the body uses the parameters: name them a0, a1, ... (plain types only; anything else keeps the declaration)
            parts = [p.strip() for p in args.split(",")] if args.strip() and args.strip() != "void" else []
            if "(" in args: body = None
            else:
                named_args = ", ".join(f"{p} a{k}" for k, p in enumerate(parts))
                body = body.replace("@ARGS@", "".join(", " + p for p in parts)).replace("@CALL@", "".join(f", a{k}" for k in range(len(parts)))).replace("@NAMED@", "")
                if body: return f"    virtual {ret} {name}({named_args}){' const' if const else ''}{' APPLE_KEXT_OVERRIDE' if override else ''} {body}   // inline in Apple's header on this release (body read from the family)"
        if body: return f"    virtual {ret} {name}({args}){' const' if const else ''}{' APPLE_KEXT_OVERRIDE' if override else ''} {body}   // inline in Apple's header on this release (body read from the family)"
        return f"    virtual {ret} {name}({args}){' const' if const else ''}{' = 0' if i in P else ''}{' APPLE_KEXT_OVERRIDE' if override else ''};"
    # overrides inside the base's range that this class itself defines and the base left pure or defined in the family
    for i in range(min(start, len(S))):
        if S[i].startswith("__ZN" + cm) or S[i].startswith("__ZNK" + cm):
            bslot = tables[base]["slots"][i] if i < len(tables[base]["slots"]) else ""
            if base in named and bslot == "___cxa_pure_virtual" and N[i]:
                d_ = dem[N[i]]; m_ = re.match(r"X::([A-Za-z_][A-Za-z0-9_]*)\((.*)\)( const)?$", d_)
                key = m_ and (m_.group(1), norm(m_.group(2)), bool(m_.group(3)))
                if key and key in emitted.get(base, {}):
                    hand.setdefault(key, emitted[base][key]); lines.append(decl(i, True))
    for i in range(start, len(S)): lines.append(decl(i, False))
    emitted[c] = dict(emitted.get(base, {}))
    for ln in lines:
        mm = re.match(r"\s*virtual\s+(.+?)\s*\b([A-Za-z_][A-Za-z0-9_]*)\((.*?)\)(\s*const)?", ln)
        if mm: emitted[c][(mm.group(2), norm(mm.group(3)), bool(mm.group(4)))] = mm.group(1).strip()
    # C: non-virtual members the newer headers declare are kept when this release's binary defines them
    keepnv = []
    for o_ in olds:
        hp = os.path.join(src, os.path.dirname(hdr), o_ + ".h")
        if not os.path.exists(hp): continue
        inside = False; incomment = False
        for ln in as_newest(open(hp).read()).split("\n"):
            if incomment:
                if "*/" in ln: incomment = False
                continue
            if "/*" in ln and "*/" not in ln:
                incomment = True
                ln = ln.split("/*")[0]
            if re.match(r"\s*class %s\s*:\s*public" % o_, ln): inside = True; continue
            if inside and ln.startswith("};"): inside = False
            if not inside: continue
            t_ = ln.strip()
            if not t_ or t_.startswith(("virtual", "OSDeclare", "public:", "private:", "protected:", "uint8_t _opaque", "//", "/*", "*", "NM_", "#")): continue
            t_ = re.sub(r"/\*.*?\*/", "", t_).split("//")[0].strip()
            if not re.match(r"^(static\s+)?[A-Za-z_][\w\s\*&:<>,]*\([^;{}]*\)\s*(const)?\s*;$", t_): continue
            mm = re.search(r"\b([A-Za-z_][A-Za-z0-9_]*)\s*\(", t_)
            if mm and "(" in t_ and t_.endswith(";"):
                nm_ = mm.group(1); tag_ = "%d%s%d%s" % (len(c), c, len(nm_), nm_)
                has_c = any(sym.startswith("__ZNK" + tag_) for sym in famsyms); has_n = any(sym.startswith("__ZN" + tag_) for sym in famsyms)
                if has_n and not has_c: t_ = re.sub(r"\)\s*const\s*;$", ");", t_)
                elif has_c and not has_n and not re.search(r"\)\s*const\s*;$", t_): t_ = t_[:-1] + " const;"
                if has_c or has_n: keepnv.append("    " + t_)
                else: dropped_nv.append(f"{c}::{nm_}")
    era_methods = set()
    for x_ in N:
        if x_ and x_ in dem:
            m2 = re.match(r"X::([A-Za-z_][A-Za-z0-9_]*)\(", dem[x_])
            if m2: era_methods.add(m2.group(1))
    seen_nv = set()
    for o_ in olds:
        hp = os.path.join(src, os.path.dirname(hdr), o_ + ".h")
        if not os.path.exists(hp): continue
        for ln in as_newest(open(hp).read()).split("\n"):
            mm = re.match(r"\s*virtual\s+(.+?)\s*\b([A-Za-z_][A-Za-z0-9_]*)\((.*?)\)(\s*const)?\s*(= 0)?\s*(APPLE_KEXT_OVERRIDE)?\s*;", ln)
            if not mm: continue
            nm_ = mm.group(2)
            if nm_ in era_methods or nm_ in seen_nv or nm_.startswith("_RESERVED"): continue
            if any(("%d%s%d%s" % (len(c), c, len(nm_), nm_)) in sym for sym in famsyms):
                keepnv.append(f"    {mm.group(1).strip()} {nm_}({mm.group(3)}){' const' if mm.group(4) else ''};   // a plain member on this release"); seen_nv.add(nm_)
    lines += keepnv
    size = t.get("size")
    body = [f"enum {{ k{c}Size = {hex(size)} }};", f"class {c} : public {base} {{", f"    OSDeclareDefaultStructors({c});", "public:"] + lines + \
           ["private:", f"    uint8_t _opaque[k{c}Size - sizeof({base})];", "};", f'static_assert(sizeof({c}) == k{c}Size, "{c} must be the size this release registers");']
    bodies[hdr] = (c, olds, "\n".join(body)); report.append(f"{c}: base {base} ({start} slots), {len(S) - start} new virtuals, {synth} with no declaration to copy (declared void), {unnamed} unnamed pure, size {size}")
os.makedirs(os.path.join(out, "iofam"), exist_ok=True); os.makedirs(os.path.join(out, "re"), exist_ok=True)
def replace_class(text, old_names, new_body):
    """cut every class the old header declares (and its size enum and assert) and put the regenerated class there"""
    done = False; res = []; lines = text.split("\n"); i = 0
    while i < len(lines):
        l = lines[i]
        if re.match(r"\s*(#if|#else|#endif).*", l) and any(re.search(r"\bk%sSize\b" % o, lines[min(i + 1, len(lines) - 1)]) for o in old_names): i += 1; continue
        if any(re.match(r"\s*enum \{ k%sSize\b" % o, l) for o in old_names): i += 1; continue
        if any(re.match(r"\s*class %s\s*:\s*public" % o, l) for o in old_names):
            while i < len(lines) and not lines[i].startswith("};"): i += 1
            i += 1
            while i < len(lines) and (lines[i].startswith("static_assert") or lines[i].strip().startswith('"') or lines[i].strip().startswith("#endif") and False): 
                # multi-line asserts end at the line holding ");"
                e = lines[i]; i += 1
                if e.rstrip().endswith(");"): break
            if not done: res.append(new_body); done = True
            continue
        res.append(l); i += 1
    return "\n".join(res), done
written = 0
for hdr, (c, olds, body) in bodies.items():
    text = as_newest(open(os.path.join(src, hdr)).read()); new, ok = replace_class(text, olds, body)
    if not ok: print("STOP: no class found to replace in", hdr); sys.exit(1)
    alias = "".join(f"\n#define {o} {c}" for o in olds if o != c)
    open(os.path.join(out, hdr), "w").write("class IOAccelSurface2;\nclass IOAccelDisplayPipe2;\nclass IOAccelDisplayMachine2;\n" + new + alias + "\n"); written += 1
for hdr, inc in EMPTY.items():
    open(os.path.join(out, hdr), "w").write(f'#pragma once\n// On this release the class is the single one declared in {inc}.\n#include "{inc}"\n'); written += 1
# everything else is copied as is; the shared types header gains forward declarations for names only this release uses
for d in ("iofam", "re"):
    for f in os.listdir(os.path.join(src, d)):
        p = f"{d}/{f}"
        if p in bodies or p in EMPTY: continue
        t = as_newest(open(os.path.join(src, p)).read())
        if p == "iofam/iofam_types.h":
            known = set(re.findall(r"^(?:class|struct)\s+(\w+);", t, re.M)) | set(re.findall(r"^typedef .*?\b(\w+);", t, re.M))
            extra = sorted(x for x in fwd if x not in known and not x.startswith(("IOAccelSurface", "IOAccelDisplayPipe", "IOAccelDisplayMachine")) and x not in ("OSSet", "OSObject", "IOService", "IOFramebuffer", "IOMemoryDescriptor", "task", "IOUserClient", "OSDictionary", "OSArray", "IOInterruptEventSource", "OSString", "OSSymbol", "IOWorkLoop", "IOPCIDevice", "IOSurface", "IOTimerEventSource", "IOCommandGate", "OSData", "IOMemoryMap", "IOExternalMethodArguments", "IOExternalMethodDispatch", "OSNumber", "IORegistryEntry"))
            # the stereo mode is a named enum in Apple's signatures, so it must be one here for the names to match
            t = t.replace("typedef unsigned int IOAccelStereoMode;", "enum IOAccelStereoMode : unsigned int { kIOAccelStereoModeNone = 0 };")
            # same for the swap request: these releases name the enum in the method's signature
            assert "typedef unsigned int eDoSwap;" in t, "swap request type not found in the shared types header"
            t = t.replace("typedef unsigned int eDoSwap;", "enum eDoSwap : unsigned int { kDoSwapNone = 0 };")
            t += "\n// names only this release's tables use\n" + "".join(f"class {x};\n" for x in extra) + "class IOAccelSurface2;\nclass IOAccelDisplayPipe2;\nclass IOAccelDisplayMachine2;\n"
        open(os.path.join(out, p), "w").write(t); written += 1
print(f"era {era}: {written} headers written to {out}")
for r in report: print("  " + r)
if revfile: json.dump([[list(k), v] for k, v in revealed.items()], open(revfile, "w"))
print("non-virtual members of the newer headers this release does not define (left out):", sorted(set(dropped_nv)))
print("methods given the body the family defines inline:", sorted(set(inlined)))
