#!/usr/bin/env python3
"""nvmtl-gpus: list every NVIDIA GPU the Mac sees, with its generation, the PCIe link it negotiated against what the card
and its slot allow, whether it sits behind Thunderbolt (an external GPU), and whether the NullMoth driver is attached.

  python3 tools/nvmtl_gpus.py            human table
  python3 tools/nvmtl_gpus.py --json     the same, machine readable
  python3 tools/nvmtl_gpus.py --selftest

Reads only the I/O Registry (ioreg); changes nothing. A link below what the card supports is flagged, because a GPU in
an x4 slot, a riser, or a Thunderbolt enclosure runs at a fraction of its bandwidth and that is the usual reason a
fast card benchmarks slow."""
import json, plistlib, subprocess, sys

# generation by device id, the same ranges as plugin/nvmtl_dlss.c
GENERATIONS = [(0x2900, 0x2fff, "Blackwell"), (0x2600, 0x28ff, "Ada"), (0x2200, 0x25ff, "Ampere"), (0x2000, 0x20ff, "Ampere"),
               (0x2180, 0x21ff, "Turing"), (0x1e00, 0x1fff, "Turing")]
SPEED = {1: 2.5, 2: 5.0, 3: 8.0, 4: 16.0, 5: 32.0, 6: 64.0}   # GT/s by the PCIe link-speed field
TUNNEL = ("Thunderbolt", "USB4")                                # class-name fragments of a tunnelled PCIe path
DRIVER = ("NVRM", "NVAccel", "NVRMFB")

def generation(dev):
    for lo, hi, name in GENERATIONS:
        if lo <= dev <= hi: return name
    return "older than Turing (not supported)" if dev < 0x1e00 else "unknown"

def decode_link(status, caps):
    """status = Link Status register, caps = Link Capabilities register. Returns the negotiated and maximum speed (GT/s) and
    width, and a verdict: 'ok'; 'degraded' (fewer lanes than the card supports); 'idle-or-low' (full width, lower speed: normal at
    idle, a fault only if it stays low under load); or 'unknown' (registers unreadable)."""
    if status is None or caps is None: return {"verdict": "unknown"}
    cur_s, cur_w = SPEED.get(status & 0xf), (status >> 4) & 0x3f
    max_s, max_w = SPEED.get(caps & 0xf), (caps >> 4) & 0x3f
    if not (cur_s and cur_w and max_s and max_w): return {"verdict": "unknown"}
    # a narrower link is a real shortfall; a lower speed at full width is normal at idle (the card steps its link down)
    verdict = "degraded" if cur_w < max_w else "idle-or-low" if cur_s < max_s else "ok"
    return {"speed": cur_s, "width": cur_w, "max_speed": max_s, "max_width": max_w, "verdict": verdict}

def word(v):
    """A 16/32-bit register property: an int, or little-endian data."""
    if isinstance(v, bytes): return int.from_bytes(v[:4], "little")
    return v if isinstance(v, int) else None

def gpus(tree):
    out = []
    def walk(node, chain):
        cls = node.get("IOObjectClass", "")
        vendor, dev = word(node.get("vendor-id")), word(node.get("device-id"))
        code = word(node.get("class-code"))
        if vendor == 0x10de and dev is not None and code is not None and (code >> 16) in (0x03,):
            kids = [c.get("IOObjectClass", "") for c in node.get("IORegistryEntryChildren", [])]
            seen = list(kids)
            def collect(n):
                for c in n.get("IORegistryEntryChildren", []):
                    seen.append(c.get("IOObjectClass", "")); collect(c)
            collect(node)
            chain_names = [n.get("IOObjectClass", "") for n in chain]
            out.append({
                "device_id": "%04X" % dev, "generation": generation(dev),
                "bus": node.get("pcidebug", ""),
                "link": decode_link(word(node.get("IOPCIExpressLinkStatus")), word(node.get("IOPCIExpressLinkCapabilities"))),
                "external": any(t in c for c in chain_names for t in TUNNEL),
                "driver": sorted({d for d in DRIVER for s in seen if s.startswith(d)}),
            })
        for c in node.get("IORegistryEntryChildren", []): walk(c, chain + [node])
    for root in (tree if isinstance(tree, list) else [tree]): walk(root, [])
    return out

def line(g):
    l = g["link"]
    if l["verdict"] == "unknown": link = "link unreadable"
    else:
        link = "PCIe %g GT/s x%d" % (l["speed"], l["width"])
        if l["verdict"] == "degraded": link += "  LOW: the card supports %g GT/s x%d" % (l["max_speed"], l["max_width"])
        if l["verdict"] == "idle-or-low": link += "  (the card supports %g GT/s; links step down at idle, check under load)" % l["max_speed"]
    return "%s  %-10s %s  %s%s  driver: %s" % (g["device_id"], g["generation"], g["bus"] or "-", link,
                                               "  [external, Thunderbolt]" if g["external"] else "", ", ".join(g["driver"]) or "not attached")

def selftest():
    def reg(speed, width): return speed | (width << 4)
    fails = []
    def arm(what, ok):
        print(("ok    " if ok else "FAIL  ") + what)
        if not ok: fails.append(what)
    arm("Gen4 x16 negotiated against a Gen4 x16 card is ok", decode_link(reg(4, 16), reg(4, 16))["verdict"] == "ok")
    arm("Gen3 x4 against a Gen4 x16 card is flagged degraded", decode_link(reg(3, 4), reg(4, 16))["verdict"] == "degraded")
    arm("a width-only shortfall (x8 on an x16 card) is flagged", decode_link(reg(4, 8), reg(4, 16))["verdict"] == "degraded")
    arm("a speed-only shortfall (Gen1 idle on a Gen5 card) is 'idle-or-low', not a fault", decode_link(reg(1, 16), reg(5, 16))["verdict"] == "idle-or-low")
    arm("unreadable registers are 'unknown', never 'ok'", decode_link(None, reg(4, 16))["verdict"] == "unknown" and decode_link(0, 0)["verdict"] == "unknown")
    arm("device ids map to generations: 2D05 Blackwell, 2684 Ada, 2504 Ampere, 1F08 Turing, 1C03 not supported",
        [generation(i) for i in (0x2d05, 0x2684, 0x2504, 0x1f08)] == ["Blackwell", "Ada", "Ampere", "Turing"] and "not supported" in generation(0x1c03))
    le = lambda v: v.to_bytes(4, "little")
    gpu = {"IOObjectClass": "IOPCIDevice", "vendor-id": le(0x10de), "device-id": le(0x2d05), "class-code": le(0x030000), "pcidebug": "1:0:0",
           "IOPCIExpressLinkStatus": reg(5, 4), "IOPCIExpressLinkCapabilities": reg(5, 16),
           "IORegistryEntryChildren": [{"IOObjectClass": "NVRM", "IORegistryEntryChildren": [{"IOObjectClass": "NVAccel"}]}]}
    other = {"IOObjectClass": "IOPCIDevice", "vendor-id": le(0x1002), "device-id": le(0x73bf), "class-code": le(0x030000)}
    audio = {"IOObjectClass": "IOPCIDevice", "vendor-id": le(0x10de), "device-id": le(0x22eb), "class-code": le(0x040300)}
    internal = {"IOObjectClass": "IOPCIBridge", "IORegistryEntryChildren": [gpu, other, audio]}
    external = {"IOObjectClass": "IOThunderboltPort", "IORegistryEntryChildren": [{"IOObjectClass": "IOPCIBridge", "IORegistryEntryChildren": [dict(gpu, **{"pcidebug": "7:0:0"})]}]}
    found = gpus([{"IOObjectClass": "IOPlatformExpertDevice", "IORegistryEntryChildren": [internal, external]}])
    arm("only NVIDIA display-class devices are listed (the AMD card and the card's own audio function are not)", len(found) == 2)
    arm("a card under a Thunderbolt port is marked external, one under the PCIe bridge is not",
        [g["external"] for g in found] == [False, True])
    arm("the attached driver components are read from the card's children", found[0]["driver"] == ["NVAccel", "NVRM"])
    arm("a Gen5 x4 link on a Gen5 x16 card prints the LOW warning with the card's maximum", "LOW" in line(found[0]) and "32 GT/s x16" in line(found[0]))
    print("FAILED %d" % len(fails) if fails else "PASS all arms")
    return 1 if fails else 0

def main():
    if "--selftest" in sys.argv: return selftest()
    raw = subprocess.run(["/usr/sbin/ioreg", "-a", "-l", "-p", "IOService"], capture_output=True, check=True).stdout
    found = gpus(plistlib.loads(raw))
    if "--json" in sys.argv: print(json.dumps(found, indent=1))
    elif not found: print("no NVIDIA display GPU in the I/O Registry")
    else:
        for g in found: print(line(g))
    return 0

if __name__ == "__main__":
    sys.exit(main())
