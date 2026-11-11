#!/usr/bin/env python3
# named.py <family dump> <out json> <vendor dump>...: fill in the names of the slots the family leaves pure, from the
# vendor drivers that implement them (a vendor class's table extends its family base slot for slot).
import json, sys, re
fam = json.load(open(sys.argv[1])); vend = {}
for f in sys.argv[3:]:
    for k, v in json.load(open(f)).items(): vend[f.split("/")[-1].replace(".json", "") + ":" + k] = v
def meth(n):
    m = re.match(r"__ZN(K?)(\d+)", n)
    if not m: return None
    return m.group(1) + n[m.end() + int(m.group(2)):]          # const marker + method name and arguments, class removed
out = {}; unresolved = 0; total_pure = 0
for c, cv in fam.items():
    S = cv["slots"]; pure = [i for i, s in enumerate(S) if s == "___cxa_pure_virtual"]
    names = [meth(s) if s != "___cxa_pure_virtual" else None for s in S]
    donors = []
    famdef = [i for i, x in enumerate(S) if re.match(r"__ZNK?\d+(IOAccel|IOGraphicsAccelerator)", x)]   # slots the family itself defines
    for vk, vv in vend.items():
        V = vv["slots"]; vcls = vk.split(":", 1)[1]; vpre = ("__ZN%d%s" % (len(vcls), vcls), "__ZNK%d%s" % (len(vcls), vcls))
        if len(V) < len(S) or not famdef: continue
        # a real subclass keeps every family-defined slot or overrides it with a method of its OWN class, and fills
        # each pure slot with a method of its own class or of a class between it and the family base
        ok = all(V[i] == S[i] or V[i].startswith(vpre) or V[i] == "___cxa_pure_virtual" or not re.match(r"__ZNK?\d+(IOAccel|IOGraphicsAccelerator)", V[i]) for i in famdef)
        keep = sum(1 for i in famdef if V[i] == S[i])
        base_agree = sum(1 for i in range(len(S)) if i not in pure and S[i] == V[i])
        if ok and keep >= max(3, int(0.5 * len(famdef))) and base_agree >= int(0.7 * (len(S) - len(pure))): donors.append((keep, vk, V))
    # the family's own subclasses are the authority for a base's slot names; vendor drivers come after
    famfile = sys.argv[1].split("/")[-1].replace(".json", "")
    donors.sort(key=lambda d: (d[1].split(":")[0] == famfile, d[0]), reverse=True)
    for i in pure:
        total_pure += 1
        for agree, vk, V in donors:
            m = meth(V[i]) if V[i] != "___cxa_pure_virtual" else None
            if m: names[i] = m; break
        if names[i] is None: unresolved += 1
    out[c] = {"size": cv.get("size"), "slots": S, "names": names, "pure": pure, "donors": [d[1] for d in donors[:3]]}
json.dump(out, open(sys.argv[2], "w"), indent=0)
print(f"{len(fam)} family classes, {total_pure} pure slots, {total_pure - unresolved} named from vendor drivers, {unresolved} still unnamed")
for c in ("IOGraphicsAccelerator2", "IOAccelSurface2", "IOAccelDisplayPipe2", "IOAccelDisplayMachine2", "IOAccelGLContext2", "IOAccelResource2", "IOAccelSysMemory", "IOAccelVidMemory", "IOAccelMemoryMap", "IOAccelEventMachineFast2", "IOAccelDevice2", "IOAccelSharedUserClient2", "IOAccelShared2"):
    if c in out:
        o = out[c]; un = [i for i in o["pure"] if o["names"][i] is None]
        print(f"  {c}: {len(o['slots'])} slots, {len(o['pure'])} pure, unnamed {len(un)} {un[:8]}, donors {o['donors'][:2]}")
