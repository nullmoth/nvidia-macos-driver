#!/usr/bin/env python3
# objcov.py <otool -ov listing> <out.json>: classes (superclass, instance size, ivars, methods, adopted protocols),
# categories and protocols (required and optional methods) read from an "otool -ov" listing.
import sys, re, json
def ind(l): return len(l) - len(l.lstrip(" \t").replace("\t", "    ")) if False else len(l.expandtabs(8)) - len(l.expandtabs(8).lstrip(" "))
L = [l.rstrip("\n") for l in open(sys.argv[1], errors="replace")]
classes = {}; protos = {}; cats = []
sec = None; cur = None; meta = False; lst = None; lst_ind = -1; ivar = None; after_isa = False; in_baseprot = False; bp_ind = -1
LISTS = {"baseMethods": "imeth", "instanceMethods": "imeth", "classMethods": "cmeth", "optionalInstanceMethods": "oimeth",
         "optionalClassMethods": "ocmeth"}
def last(l): return l.split()[-1] if l.split() else ""
i = 0
while i < len(L):
    l = L[i]; s = l.strip(); i += 1
    if l.startswith("Contents of ("):
        m = re.search(r"__objc_(\w+)\)", l); sec = m.group(1) if m else None; cur = None; lst = None; continue
    if sec not in ("classlist", "protolist", "catlist", "nlclslist"): continue
    n = ind(l)
    if n == 0 and re.match(r"^[0-9a-f]{8,16} 0x[0-9a-f]+", l):
        meta = False; lst = None; ivar = None; in_baseprot = False
        if sec in ("classlist", "nlclslist"):
            m = re.search(r"_OBJC_CLASS_\$_(\S+)", l)
            cur = {"kind": "class", "name": m.group(1) if m else None, "super": None, "start": None, "size": None, "ivars": [],
                   "imeth": {}, "cmeth": {}, "protocols": []}
        elif sec == "protolist":
            cur = {"kind": "proto", "name": None, "imeth": {}, "cmeth": {}, "oimeth": {}, "ocmeth": {}, "protocols": []}
        else:
            cur = {"kind": "cat", "name": None, "cls": None, "imeth": {}, "cmeth": {}}; cats.append(cur)
        after_isa = False; continue
    if cur is None: continue
    if s == "Meta Class": meta = True; lst = None; ivar = None; in_baseprot = False; continue
    key = s.split()[0] if s else ""
    if in_baseprot and n <= bp_ind and key not in ("count", "list[0]") and not key.startswith("list["): in_baseprot = False
    if lst and n <= lst_ind: lst = None
    if ivar is not None and n <= ivar_ind: ivar = None
    if key in LISTS:
        lst = LISTS[key]; lst_ind = n
        if cur["kind"] != "proto" and meta and lst == "imeth": lst = "cmeth"      # a metaclass's baseMethods are class methods
        if in_baseprot: lst = "skip"
        continue
    if key in ("baseProtocols", "protocols") and cur["kind"] in ("class", "proto") and not in_baseprot:
        if not (cur["kind"] == "class" and meta): in_baseprot = True; bp_ind = n
        else: in_baseprot = True; bp_ind = n; 
        continue
    if key == "ivars" and cur["kind"] == "class" and not meta and not in_baseprot:
        ivar = []; ivar_ind = n; continue
    if key == "isa": after_isa = True; continue
    if lst and lst != "skip" and key == "name":
        nm = last(l); t = last(L[i]) if i < len(L) and L[i].strip().startswith("types") else ""
        if nm.startswith("(0x"):      # a relative method list prints no selector text: take it from the implementation's symbol
            for j in (i, i + 1):
                mm = re.search(r"[-+]\[\S+ (\S+)\]\s*$", L[j]) if j < len(L) and L[j].strip().startswith("imp") else None
                if mm: nm = mm.group(1); break
        cur[lst][nm] = t; continue
    if ivar is not None:
        if key == "offset": cur["ivars"].append({"offset": int(last(l)) if last(l).isdigit() else last(l)})
        elif key == "name" and cur["ivars"]: cur["ivars"][-1]["name"] = last(l)
        elif key == "type" and cur["ivars"]: cur["ivars"][-1]["type"] = s.split(None, 2)[2] if len(s.split(None, 2)) > 2 else ""
        elif key == "size" and cur["ivars"]: cur["ivars"][-1]["size"] = int(last(l)) if last(l).isdigit() else last(l)
        continue
    if in_baseprot:
        if key == "name" and after_isa and not (cur["kind"] == "class" and meta):
            if last(l) not in cur["protocols"]: cur["protocols"].append(last(l))
        after_isa = False; continue
    if key == "name" and lst is None:
        if cur["kind"] == "proto" and cur["name"] is None: cur["name"] = last(l); protos[cur["name"]] = cur
        elif cur["kind"] == "class" and not meta:
            if cur["name"] is None: cur["name"] = last(l)
            classes[cur["name"]] = cur
        elif cur["kind"] == "cat" and cur["name"] is None: cur["name"] = last(l)
    elif key == "superclass" and cur["kind"] == "class" and not meta:
        m = re.search(r"_OBJC_CLASS_\$_(\S+)", l); cur["super"] = m.group(1) if m else last(l)
    elif key == "cls" and cur["kind"] == "cat":
        m = re.search(r"_OBJC_CLASS_\$_(\S+)", l); cur["cls"] = m.group(1) if m else last(l)
    elif key == "instanceStart" and not meta: cur["start"] = int(last(l))
    elif key == "instanceSize" and not meta: cur["size"] = int(last(l))
    after_isa = False
json.dump({"classes": classes, "protocols": protos, "categories": cats}, open(sys.argv[2], "w"))
print(sys.argv[1].split("/")[-1], "classes", len(classes), "protocols", len(protos), "categories", len(cats))
