#!/usr/bin/env python3
# Classify every unguarded-availability site of the plugin by what it does at run time.
# usage: classify.py <src dir> <mesa include> <floor> <out.tsv>   (env LIBCLANG = path of libclang.dylib,
# CINDEX = folder holding the clang python package, SDKROOT = macOS SDK)
import os, re, sys, collections
sys.path.insert(0, os.environ["CINDEX"])
from clang import cindex
from clang.cindex import CursorKind as K
cindex.Config.set_library_file(os.environ["LIBCLANG"])
SRC, MESA, FLOOR, OUT = sys.argv[1:5]
SDK = os.environ["SDKROOT"]
os.chdir(SRC)
TUS = ["NVMTLDevice.m", "nvmtl_dlss.c", "dlss/elfhost.c", "dlss/lnxlibc.c"]
ARGS = ["-target", "x86_64-apple-macos" + FLOOR, "-isysroot", SDK, "-fobjc-arc", "-DNVMTL_RELEASE=1", "-Wno-protocol",
        "-Wno-incomplete-implementation", "-Wunguarded-availability", "-ferror-limit=0", "-I" + MESA]
if os.environ.get("CLANG_RESOURCE"): ARGS += ["-resource-dir", os.environ["CLANG_RESOURCE"]]
idx = cindex.Index.create()
import ctypes
class CXVersion(ctypes.Structure): _fields_ = [("major", ctypes.c_int), ("minor", ctypes.c_int), ("sub", ctypes.c_int)]
class CXPA(ctypes.Structure): _fields_ = [("Platform", cindex._CXString), ("Introduced", CXVersion), ("Deprecated", CXVersion),
                                          ("Obsoleted", CXVersion), ("Unavailable", ctypes.c_int), ("Message", cindex._CXString)]
_lib = cindex.conf.lib
_pa = _lib.clang_getCursorPlatformAvailability
_pa.argtypes = [cindex.Cursor, ctypes.POINTER(ctypes.c_int), ctypes.POINTER(cindex._CXString), ctypes.POINTER(ctypes.c_int),
                ctypes.POINTER(cindex._CXString), ctypes.POINTER(CXPA), ctypes.c_int]
_pa.restype = ctypes.c_int
_raw = ctypes.CDLL(os.environ["LIBCLANG"])
_cstr = _raw.clang_getCString; _cstr.argtypes = [cindex._CXString]; _cstr.restype = ctypes.c_char_p
ZERO = (10, 0, 0)
def intro1(c):
    arr = (CXPA * 8)(); a = ctypes.c_int(); b = ctypes.c_int(); m1 = cindex._CXString(); m2 = cindex._CXString()
    n = _pa(c, ctypes.byref(a), ctypes.byref(m1), ctypes.byref(b), ctypes.byref(m2), arr, 8)
    for i in range(min(n, 8)):
        if (_cstr(arr[i].Platform) or b"") == b"macos":
            v = arr[i].Introduced
            if v.major >= 0: return (v.major, max(v.minor, 0), max(v.sub, 0))
    return None
def intro(c):
    """macOS version that introduced a declaration (its own attribute, else its container's), (10,0,0) if none"""
    if c is None: return ZERO
    v = intro1(c)
    if v is None:
        p = c.semantic_parent
        if p is not None and p.kind in (K.OBJC_PROTOCOL_DECL, K.OBJC_INTERFACE_DECL, K.OBJC_CATEGORY_DECL, K.ENUM_DECL): v = intro1(p)
    return v or ZERO
_ov = _raw.clang_getOverriddenCursors
_ov.argtypes = [cindex.Cursor, ctypes.POINTER(ctypes.POINTER(cindex.Cursor)), ctypes.POINTER(ctypes.c_uint)]; _ov.restype = None
def overridden(c):
    p = ctypes.POINTER(cindex.Cursor)(); n = ctypes.c_uint(0)
    _ov(c, ctypes.byref(p), ctypes.byref(n))
    out = []
    for i in range(n.value):
        x = cindex.Cursor.from_buffer_copy(p[i]); x._tu = c._tu; out.append(x)
    return out
def vs(v): return "%d.%d" % (v[0], v[1]) + (".%d" % v[2] if v[2] else "")
def vt(s):
    p = [int(x) for x in s.split(".")]; return tuple(p + [0] * (3 - len(p)))
rows = []; errors = 0

def own(c):
    f = c.location.file
    return f is not None and os.path.realpath(f.name).startswith(os.path.realpath(SRC) + os.sep)

def walk_impl(tu):
    """selectors our translation unit implements, and property names our interfaces declare"""
    sels = collections.defaultdict(set); ifaces = {}
    for c in tu.cursor.get_children():
        if not own(c): continue
        if c.kind in (K.OBJC_IMPLEMENTATION_DECL, K.OBJC_CATEGORY_IMPL_DECL):
            for m in c.get_children():
                if m.kind in (K.OBJC_INSTANCE_METHOD_DECL, K.OBJC_CLASS_METHOD_DECL): sels[m.spelling].add(c.spelling)
                if m.kind == K.OBJC_SYNTHESIZE_DECL: sels[m.spelling].add(c.spelling)
        if c.kind in (K.OBJC_INTERFACE_DECL, K.OBJC_CATEGORY_DECL):
            sup = [x.spelling for x in c.get_children() if x.kind == K.OBJC_SUPER_CLASS_REF]
            ifaces[c.spelling] = sup[0] if sup else ifaces.get(c.spelling)
            for m in c.get_children():
                if m.kind == K.OBJC_PROPERTY_DECL:
                    sels[m.spelling].add(c.spelling + "(prop)")
                    sels["set" + m.spelling[:1].upper() + m.spelling[1:] + ":"].add(c.spelling + "(prop)")
    return sels, ifaces

def sdk_selectors(tu):
    """selector -> earliest macOS that declares it, over the Metal, MetalFX and QuartzCore headers"""
    out = {}
    for c in tu.cursor.get_children():
        f = c.location.file
        if f is None or not any(x in f.name for x in ("Metal.framework", "MetalFX.framework", "QuartzCore.framework")): continue
        if c.kind in (K.OBJC_PROTOCOL_DECL, K.OBJC_INTERFACE_DECL, K.OBJC_CATEGORY_DECL):
            for m in c.get_children():
                if m.kind in (K.OBJC_INSTANCE_METHOD_DECL, K.OBJC_CLASS_METHOD_DECL):
                    v = intro(m); out[m.spelling] = min(out.get(m.spelling, v), v)
                elif m.kind == K.OBJC_PROPERTY_DECL:
                    v = intro(m)
                    for s in (m.spelling, "set" + m.spelling[:1].upper() + m.spelling[1:] + ":"): out[s] = min(out.get(s, v), v)
    return out

def own_defs(tu):
    """extents of every function and method our sources define, with the version gate each one implies"""
    defs = []
    def add(m, owner):
        e = m.extent
        if e.start.file is None: return
        defs.append((e.start.file.name, e.start.line, e.end.line, m, owner))
    for c in tu.cursor.get_children():
        if not own(c): continue
        if c.kind in (K.OBJC_IMPLEMENTATION_DECL, K.OBJC_CATEGORY_IMPL_DECL):
            for m in c.get_children():
                if m.kind in (K.OBJC_INSTANCE_METHOD_DECL, K.OBJC_CLASS_METHOD_DECL): add(m, c.spelling)
        elif c.kind == K.FUNCTION_DECL and c.is_definition(): add(c, "")
    return defs

def gate_of(defn, sdk):
    """(version, reason): the oldest macOS on which this function or method can be entered with real arguments"""
    m = defn[3]; g = ZERO; why = "-"
    if m.kind != K.FUNCTION_DECL:
        # the method is an entry point only if it implements something its class or protocols declare; the gate is the
        # oldest macOS that declares it (an undeclared helper has no gate)
        ovs = overridden(m)
        if ovs:
            e = min(intro(o) for o in ovs)
            if e > g: g = e; why = "entry:" + m.spelling
    for p in m.get_children():
        if p.kind != K.PARM_DECL: continue
        for r in p.get_children():
            if r.kind in (K.OBJC_CLASS_REF, K.OBJC_PROTOCOL_REF, K.TYPE_REF) and r.referenced is not None:
                v = intro(r.referenced)
                if v > g: g = v; why = "param:" + r.referenced.spelling
    return g, why

def enclosing(defs, f, line):
    best = None
    for d in defs:
        if d[0] == f and d[1] <= line <= d[2] and (best is None or d[2] - d[1] < best[2] - best[1]): best = d
    return best

def first_child(c):
    for x in c.get_children(): return x
    return None

def classify(tu, d, sels, ifaces, sdk, defs):
    m = re.match(r"'(.+)' is only available on macOS ([0-9.]+) or newer", d.spelling)
    api, ver = m.group(1), m.group(2)
    loc = d.location
    cur = cindex.Cursor.from_location(tu, loc)
    kind = cur.kind.name
    ref = cur.referenced
    refkind = ref.kind.name if ref is not None else "-"
    parent = ref.semantic_parent if ref is not None else None
    cont = (parent.kind.name + ":" + parent.spelling) if parent is not None and parent.kind != K.TRANSLATION_UNIT else "-"
    recv = "-"; recvkind = "-"
    if cur.kind in (K.OBJC_MESSAGE_EXPR, K.MEMBER_REF_EXPR):
        fc = first_child(cur)
        if fc is not None:
            recv = fc.type.spelling or "-"; recvkind = fc.kind.name
            toks = [t.spelling for t in fc.get_tokens()][:1]
            if toks == ["super"]: recvkind = "SUPER"
            if toks == ["twin"]: recvkind = "APPLE_TWIN"      # the same library loaded by Apple's own compiler device
    # verdict
    sel = ref.spelling if ref is not None else api
    v = "?"
    if refkind == "ENUM_CONSTANT_DECL": v = "a:constant"
    elif cur.kind in (K.TYPE_REF,) or (cur.kind in (K.OBJC_CLASS_REF, K.OBJC_PROTOCOL_REF) and refkind in ("OBJC_INTERFACE_DECL", "OBJC_PROTOCOL_DECL", "TYPEDEF_DECL", "STRUCT_DECL", "ENUM_DECL")):
        v = "a:type"
    elif refkind in ("TYPEDEF_DECL", "STRUCT_DECL", "ENUM_DECL", "OBJC_PROTOCOL_DECL"): v = "a:type"
    elif refkind == "FUNCTION_DECL": v = "b:c-function"
    elif refkind == "VAR_DECL": v = "b:data-symbol"
    elif refkind in ("OBJC_INSTANCE_METHOD_DECL", "OBJC_CLASS_METHOD_DECL", "OBJC_PROPERTY_DECL"):
        pk = parent.kind if parent is not None else None
        if recvkind == "SUPER": v = "b:super-send"
        elif recvkind == "APPLE_TWIN": v = "b:apple-object-send"
        elif refkind == "OBJC_CLASS_METHOD_DECL": v = "b:class-send"
        elif pk == K.OBJC_PROTOCOL_DECL:
            v = "a:own-object" if sel in sels else "b:protocol-send-unimplemented"
        else:
            v = "b:apple-object-send"
    elif refkind == "OBJC_INTERFACE_DECL": v = "?:class-ref"
    if ref is not None and refkind in ("FUNCTION_DECL", "VAR_DECL") and ref.storage_class == cindex.StorageClass.STATIC:
        v = "a:inline-or-static"      # defined in the header: nothing is imported, so nothing can be missing
    if v.startswith("b:") and v.endswith("-send"):
        try: L = open(loc.file.name, errors="replace").read().split("\n")
        except Exception: L = []
        near = " ".join(L[max(0, loc.line - 4):loc.line])
        if "respondsToSelector:@selector(" + sel + ")" in near: v = "a:guarded-by-respondsToSelector"
    if cur.kind == K.OBJC_SUPER_CLASS_REF: v = "b:subclass-of-newer-class"
    if cur.kind == K.OBJC_CLASS_REF:
        try: src = open(loc.file.name, errors="replace").read().split("\n")[loc.line - 1]
        except Exception: src = ""
        before = src[:loc.column - 1].rstrip(); after = src[loc.column - 1 + len(api):].lstrip()
        if before.endswith("[") or after.startswith("."): v = "b:class-receiver"
    # gate: the oldest macOS on which this site can run
    g = ZERO; why = "-"; enc = enclosing(defs, loc.file.name, loc.line); encname = "-"
    if enc is not None:
        g, why = gate_of(enc, sdk); encname = (enc[4] + " " if enc[4] else "") + enc[3].spelling
    if v in ("b:apple-object-send",) and parent is not None:
        pv = intro1(parent) or ZERO
        if pv > g: g = pv; why = "receiver:" + parent.spelling
    V = vt(ver)
    live = "-"
    if v.startswith("b:") or v.startswith("?"): live = "live" if g < V else "entry-gated"
    return [api, ver, kind, refkind, cont, recvkind, recv, sel, v, live, vs(g), why, encname]

popl = 0
for src in TUS:
    tu = idx.parse(src, args=ARGS)
    sels, ifaces = walk_impl(tu) if src.endswith(".m") else ({}, {})
    sdk = sdk_selectors(tu); defs = own_defs(tu)
    print(src, "sdk selectors", len(sdk), "own definitions", len(defs), file=sys.stderr)
    if src.endswith(".m"): print("own selectors implemented:", len(sels), "own interfaces:", len(ifaces), file=sys.stderr)
    for d in tu.diagnostics:
        if d.severity >= 3: errors += 1; print("ERR", d.location, d.spelling, file=sys.stderr); continue
        if "is only available on macOS" not in d.spelling: continue
        popl += 1
        f = d.location.file.name; line = d.location.line; col = d.location.column
        try: text = open(f, errors="replace").read().split("\n")[line - 1].strip()[:200]
        except Exception as e: text = "unread " + str(e)
        rows.append([f.lstrip("./"), str(line), str(col)] + classify(tu, d, sels, ifaces, sdk, defs) + [text])
with open(OUT, "w") as o:
    o.write("\t".join(["file", "line", "col", "api", "introduced", "cursor", "decl", "container", "recv_kind", "recv_type", "selector", "verdict", "live", "gate", "gate_reason", "enclosing", "source"]) + "\n")
    for r in rows: o.write("\t".join(r) + "\n")
print("floor", FLOOR, "sites", popl, "errors", errors)
c = collections.Counter(r[11] + (" " + r[12] if r[12] != "-" else "") for r in rows)
for k, n in sorted(c.items()): print("  ", k, n)
