#!/usr/bin/env python3
# iface_cmp.py <plugin.json> <sdkp dir> <objc dir> <release>...: what each release asks of each plugin class.
# Public side: the required methods of every public protocol the class adopts, from that release's SDK headers.
# Private side: the required methods of the matching SPI protocols found in that release's Metal binary.
# Less what Apple's base class implements in that binary (only NVMTLDevice has an Apple base), less what the plugin
# implements. The remainder reaches the plugin's forwarder (logged, answered with zero) on that release.
import sys, json, os
plug = json.load(open(sys.argv[1]))["classes"]; SDKP, OBJC = sys.argv[2], sys.argv[3]
def allm(C, name):
    out = set()
    while name in C: out |= set(C[name]["imeth"]); name = C[name]["super"]
    return out
def pub(P, name, seen):
    if name not in P or name in seen: return set()
    seen.add(name); out = set(P[name]["req"])
    for q in P[name]["parents"]: out |= pub(P, q, seen)
    return out
def spi(P, name, seen):
    if name not in P or name in seen: return set()
    seen.add(name); out = set(P[name]["imeth"])
    for q in P[name]["protocols"]:
        if q.endswith("SPI") or q.endswith("Implementation"): out |= spi(P, q, seen)
    return out
NSO = {"retain", "release", "autorelease", "retainCount", "zone", "hash", "superclass", "class", "self", "isEqual:", "description",
       "debugDescription", "performSelector:", "performSelector:withObject:", "performSelector:withObject:withObject:", "isProxy",
       "isKindOfClass:", "isMemberOfClass:", "conformsToProtocol:", "respondsToSelector:", "copyWithZone:", "dealloc", "init"}
def compute(rel):
    sdk = json.load(open("%s/%s.json" % (SDKP, rel)))
    bp = "%s/Metal-%s.json" % (OBJC, rel)
    d = json.load(open(bp)) if os.path.exists(bp) else {"classes": {}, "protocols": {}, "categories": []}
    C, P = d["classes"], d["protocols"]
    catm = {}
    for c in d["categories"]: catm.setdefault(c["cls"], set()).update(c["imeth"])
    rows = []
    for cname, c in sorted(plug.items()):
        mine = allm(plug, cname); base = set()
        if c["super"] and c["super"] not in plug and c["super"] != "NSObject":
            base = allm(C, c["super"]); s = c["super"]
            while s in C: base |= catm.get(s, set()); s = C[s]["super"]
        for pr in c["protocols"]:
            if not pr.startswith("MTL") or pr.endswith("SPI"): continue
            rp = pub(sdk, pr, set()) - NSO
            rs = set()
            for v in (pr + "SPI", pr + "Implementation"): rs |= spi(P, v, set())
            rs -= rp | NSO
            if not rp and not rs: continue
            rows.append((cname, pr, rp, rs, rp - base - mine, rs - base - mine))
    return rows, bool(C)
# the control: the newest system, where the plugin runs today. A method missing there too is a gap the plugin already
# lives with (its forwarder answers it); only what is missing on an older release and NOT in the control is new.
ctl_rows, ctl_bin = compute(sys.argv[4])
ctl_pub = {(r[0], r[1]): r[4] for r in ctl_rows}; ctl_spi = {(r[0], r[1]): r[5] for r in ctl_rows}
ctl_spi_all = set().union(*[r[3] for r in ctl_rows]) if ctl_rows else set()
print("control %s: public required %d missing %d | SPI required %d missing %d (binary read: %s)" % (sys.argv[4], sum(len(r[2]) for r in ctl_rows),
      sum(len(r[4]) for r in ctl_rows), sum(len(r[3]) for r in ctl_rows), sum(len(r[5]) for r in ctl_rows), "yes" if ctl_bin else "no"))
for rel in sys.argv[5:]:
    rows, hasbin = compute(rel)
    newp = {(r[0], r[1]): sorted(r[4] - ctl_pub.get((r[0], r[1]), set())) for r in rows}
    news = {(r[0], r[1]): sorted(r[5] - ctl_spi.get((r[0], r[1]), set())) for r in rows}
    gone = {(r[0], r[1]): sorted(x for x in news[(r[0], r[1])] if x not in ctl_spi_all) for r in rows}
    print("== release %s: public required %d, missing %d, of those not missing in the control %d | SPI required %d, missing %d, not missing in the control %d, of those absent from the control's SPI altogether %d (binary read: %s)" % (
          rel, sum(len(r[2]) for r in rows), sum(len(r[4]) for r in rows), sum(len(v) for v in newp.values()),
          sum(len(r[3]) for r in rows), sum(len(r[5]) for r in rows), sum(len(v) for v in news.values()), sum(len(v) for v in gone.values()), "yes" if hasbin else "no"))
    for r in rows:
        k = (r[0], r[1])
        if newp[k]: print("     %s <%s> public, new against the control: %s" % (r[0], r[1], " ".join(newp[k])))
        if news[k]: print("     %s <%s> SPI, new against the control (%d): %s" % (r[0], r[1], len(news[k]), " ".join(news[k])))
