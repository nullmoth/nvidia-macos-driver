#!/usr/bin/env python3
"""nvfb-edid.py: make an EDID override for NVRMFB's feature/change-resolution (fork).

  nvfb-edid.py <display EDID .bin> <out .bin> WxH@HZ [WxH@HZ ...]     e.g. 1920x1080@144 1920x1080@240 2560x1440@144
  nvfb-edid.py --dump <out .bin>                                      the display's current EDID, read from ioreg (macOS)

Appends one CTA-861 extension block holding up to six extra detailed timings (VESA CVT reduced blanking v2) to the
display's own EDID, and fixes the extension count and checksums. Install the result as
  /Library/NullMoth/edid-override/<vendor>-<product>.bin   (root:wheel, 0644; the name is printed below)
and restart: NVRMFB hands it to NVKMS with every display query, so NVKMS validates the new timings (or refuses those
the link cannot carry) and macOS lists them. Delete the file and restart to go back.
"""
import math, re, subprocess, sys

def cvt_rb2(w, h, hz):
    """CVT 1.2 reduced blanking v2 totals (fixed 80-pixel horizontal blanking, at least 460 us of vertical blanking) and
    pixel clock. The vertical blanking is laid out as a 3-line front porch, an 8-line sync and the rest as back porch:
    strict RB2 puts the excess in the front porch, which overflows the 6 bits a detailed timing has for it at high rates."""
    hfront, hsync, hblank = 8, 32, 80
    vsync, vback, vfront_min = 8, 6, 1
    h_period = (1.0 / hz - 460e-6) / h                      # seconds per line, first estimate
    vbi = max(math.floor(460e-6 / h_period) + 1, vfront_min + vsync + vback)
    htotal, vtotal = w + hblank, h + vbi
    pclk_khz = round(hz * htotal * vtotal / 1000.0)
    return dict(w=w, h=h, hblank=hblank, vblank=vbi, hfront=hfront, hsync=hsync, vfront=3,
                vsync=vsync, pclk10k=round(pclk_khz / 10), hpos=True, vpos=False)

def dtd(t, mm=(0, 0)):
    p = t["pclk10k"]
    if not 0 < p < 65536: raise SystemExit(f"{t['w']}x{t['h']}: pixel clock {p * 10} kHz does not fit a DTD")
    b = bytearray(18)
    b[0:2] = p.to_bytes(2, "little")
    b[2] = t["w"] & 255; b[3] = t["hblank"] & 255; b[4] = ((t["w"] >> 8) << 4) | (t["hblank"] >> 8)
    b[5] = t["h"] & 255; b[6] = t["vblank"] & 255; b[7] = ((t["h"] >> 8) << 4) | (t["vblank"] >> 8)
    b[8] = t["hfront"] & 255; b[9] = t["hsync"] & 255
    if t["hfront"] > 1023 or t["hsync"] > 1023 or t["vfront"] > 63 or t["vsync"] > 63 or t["hblank"] > 4095 or t["vblank"] > 4095:
        raise SystemExit(f"{t['w']}x{t['h']}: a porch or sync width does not fit a detailed timing")
    b[10] = ((t["vfront"] & 15) << 4) | (t["vsync"] & 15)
    b[11] = (((t["hfront"] >> 8) & 3) << 6) | (((t["hsync"] >> 8) & 3) << 4) | (((t["vfront"] >> 4) & 3) << 2) | ((t["vsync"] >> 4) & 3)
    b[12] = mm[0] & 255; b[13] = mm[1] & 255; b[14] = ((mm[0] >> 8) << 4) | (mm[1] >> 8)
    b[17] = 0x18 | (0x04 if t["vpos"] else 0) | (0x02 if t["hpos"] else 0)   # digital separate sync
    return bytes(b)

def checksum(block):
    block[127] = (-sum(block[:127])) & 255

def dump(out):
    io = subprocess.run(["ioreg", "-l", "-w0"], capture_output=True, text=True).stdout
    m = re.search(r'"IODisplayEDID" = <([0-9a-f]+)>', io)
    if not m: raise SystemExit("no IODisplayEDID in ioreg")
    open(out, "wb").write(bytes.fromhex(m.group(1)))
    print(f"{out}: {len(m.group(1)) // 2} bytes")

def main(a):
    if len(a) == 2 and a[0] == "--dump": return dump(a[1])
    if len(a) < 3: raise SystemExit(__doc__)
    e = bytearray(open(a[0], "rb").read())
    if len(e) < 128 or len(e) % 128 or e[:8] != b"\x00\xff\xff\xff\xff\xff\xff\x00": raise SystemExit("not an EDID")
    if len(e) // 128 >= 4: raise SystemExit("the EDID already has 3 extension blocks")
    modes = []
    for m in a[2:]:
        r = re.fullmatch(r"(\d+)x(\d+)@(\d+)", m)
        if not r: raise SystemExit(f"bad mode {m} (want WxH@HZ)")
        modes.append(tuple(int(x) for x in r.groups()))
    if len(modes) > 6: raise SystemExit("at most 6 modes fit one extension block")
    ext = bytearray(128); ext[0], ext[1], ext[2], ext[3] = 0x02, 0x03, 4, 0x00   # CTA-861 rev 3, DTDs from byte 4, no data blocks
    o = 4
    for w, h, hz in modes:
        t = cvt_rb2(w, h, hz)
        ext[o:o + 18] = dtd(t); o += 18
        print(f"  {w}x{h}@{hz}: total {w + t['hblank']}x{h + t['vblank']}, pixel clock {t['pclk10k'] / 100:.2f} MHz (CVT-RB2)")
    checksum(ext)
    e[126] += 1
    base = bytearray(e[:128]); checksum(base); e[:128] = base
    e += ext
    open(a[1], "wb").write(e)
    print(f"{a[1]}: {len(e)} bytes; install as /Library/NullMoth/edid-override/{e[8]:02X}{e[9]:02X}-{e[11]:02X}{e[10]:02X}.bin")

if __name__ == "__main__":
    main(sys.argv[1:])
