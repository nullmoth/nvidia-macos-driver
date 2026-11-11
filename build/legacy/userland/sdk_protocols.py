#!/usr/bin/env python3
# sdk_protocols.py <sdk path> <floor> <out.json>: every Objective-C protocol the SDK's Metal headers declare, with its
# required and optional instance methods (property accessors included) and the protocols it inherits.
# Environment as classify.py: LIBCLANG, CINDEX, CLANG_RESOURCE.
import os, sys, json, ctypes, tempfile
sys.path.insert(0, os.environ["CINDEX"])
from clang import cindex
from clang.cindex import CursorKind as K
cindex.Config.set_library_file(os.environ["LIBCLANG"])
SDK, FLOOR, OUT = sys.argv[1:4]
raw = ctypes.CDLL(os.environ["LIBCLANG"])
opt = raw.clang_Cursor_isObjCOptional; opt.argtypes = [cindex.Cursor]; opt.restype = ctypes.c_uint
pattr = raw.clang_Cursor_getObjCPropertyAttributes; pattr.argtypes = [cindex.Cursor, ctypes.c_uint]; pattr.restype = ctypes.c_uint
cstr = raw.clang_getCString; cstr.argtypes = [cindex._CXString]; cstr.restype = ctypes.c_char_p
gn = raw.clang_Cursor_getObjCPropertyGetterName; gn.argtypes = [cindex.Cursor]; gn.restype = cindex._CXString
sn = raw.clang_Cursor_getObjCPropertySetterName; sn.argtypes = [cindex.Cursor]; sn.restype = cindex._CXString
src = tempfile.NamedTemporaryFile(suffix=".m", delete=False, dir=os.path.dirname(OUT)); src.write(b"#import <Metal/Metal.h>\n"); src.close()
args = ["-target", "x86_64-apple-macos" + FLOOR, "-isysroot", SDK, "-fobjc-arc", "-ferror-limit=0", "-Wno-everything"]
if os.environ.get("CLANG_RESOURCE"): args += ["-resource-dir", os.environ["CLANG_RESOURCE"]]
tu = cindex.Index.create().parse(src.name, args=args)
errs = [d.spelling for d in tu.diagnostics if d.severity >= 3]
P = {}
for c in tu.cursor.get_children():
    f = c.location.file
    if c.kind != K.OBJC_PROTOCOL_DECL or f is None or "Metal.framework" not in f.name: continue
    if not c.is_definition(): continue
    p = P.setdefault(c.spelling, {"req": [], "opt": [], "parents": []})
    for m in c.get_children():
        if m.kind == K.OBJC_PROTOCOL_REF: p["parents"].append(m.spelling)
        elif m.kind == K.OBJC_INSTANCE_METHOD_DECL: p["opt" if opt(m) else "req"].append(m.spelling)
        elif m.kind == K.OBJC_PROPERTY_DECL:
            names = [(cstr(gn(m)) or b"").decode()]
            if not pattr(m, 0) & 1: names.append((cstr(sn(m)) or b"").decode())
            for n in names:
                if n: p["opt" if opt(m) else "req"].append(n)
os.unlink(src.name)
json.dump(P, open(OUT, "w"))
print(os.path.basename(SDK), "floor", FLOOR, "protocols", len(P), "methods", sum(len(v["req"]) + len(v["opt"]) for v in P.values()), "parse errors", len(errs), errs[:3])
