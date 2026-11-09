#!/usr/bin/env python3
"""nullmoth_settings: the driver's control panel as a command line (and the engine a Mac-app panel sits on).

The Windows driver keeps HDR, 10-bit colour, variable refresh, scaling, DSR, surround, colour range and frame limits in
one control panel. Here each of them is a boot argument or a line in /Library/GPUBundles/nvmtl/nvrm610.conf; this tool
is the one place that knows their names, their allowed values and how to change them without losing anything else.

  python3 tools/nullmoth_settings.py list                      every setting, its group, current value and what it does
  python3 tools/nullmoth_settings.py get  hdr
  python3 tools/nullmoth_settings.py set  hdr on               shows what would change; changes nothing
  python3 tools/nullmoth_settings.py set  hdr on --apply       writes it (needs sudo); a boot-arg change needs a restart
  python3 tools/nullmoth_settings.py set  hdr default --apply  removes the setting (the driver decides)
  python3 tools/nullmoth_settings.py --selftest

Rules it keeps:
  - a boot argument is added, replaced or removed on its own; every other boot argument stays exactly as it was;
  - a conf setting goes inside the user's own block ("my settings, kept on update"), the only part an update keeps;
  - a value outside its allowed range is refused before anything is written;
  - nothing is written without --apply, and conf writes are atomic (temp file, then rename).
"""
import os, re, subprocess, sys, tempfile

BLOCK_START = "# --- my settings, kept on update ---"
BLOCK_END = "# --- end my settings ---"
CONF = "/Library/GPUBundles/nvmtl/nvrm610.conf"
# the installer keeps a user-block line only if it matches this, so the tool never writes a line an update would drop
KEPT = re.compile(r"^(NVMTL|NVK|NVRM)_[A-Z0-9_]+=[A-Za-z0-9._,=*-]{0,160}$")

def S(id, group, title, where, key, kind, help, values=None, rng=None, effect="restart"):
    return dict(id=id, group=group, title=title, where=where, key=key, kind=kind, help=help, values=values, rng=rng, effect=effect)

CATALOG = [
    S("hdr", "Display", "HDR10", "bootarg", "nvfbhdr", "bool", "Advertise HDR10 to displays that list PQ in their EDID."),
    S("10bit", "Display", "10-bit colour", "bootarg", "nvfb10bit", "bool", "Scan out 30-bit surfaces on mode changes."),
    S("vrr", "Display", "Variable refresh rate (G-Sync Compatible / Adaptive-Sync)", "bootarg", "nvfbvrr", "bool",
      "Let WindowServer request variable refresh on displays that support it."),
    S("bpc", "Display", "Output colour depth", "bootarg", "nvfbbpc", "enum", "Bits per colour channel on the link; default lets the driver pick.",
      values={"6": "6", "8": "8", "10": "10", "12": "12"}),
    S("format", "Display", "Output colour format", "bootarg", "nvfbformat", "enum", "Pixel format on the link; default lets the driver pick.",
      values={"rgb": "0", "ycbcr422": "1", "ycbcr444": "2"}),
    S("range", "Display", "Output dynamic range", "bootarg", "nvfbrange", "enum", "Full or limited RGB range; default lets the driver pick.",
      values={"full": "1", "limited": "2"}),
    S("vibrance", "Display", "Digital vibrance", "bootarg", "nvfbvib", "int", "Saturation gain, 0 to 200; 100 leaves colours alone.", rng=(0, 200)),
    S("dsr", "Display", "Dynamic super resolution (render above the panel's resolution)", "bootarg", "nvfbdsr", "bool",
      "Offer desktops larger than the panel and scale them down. Reserves scanout memory."),
    S("hidpi", "Display", "HiDPI 'looks like' modes", "bootarg", "nvfbhidpi", "bool", "Back scaled modes with a desktop up to twice the panel raster."),
    S("rotscale", "Display", "Scaled modes on rotated displays", "bootarg", "nvfbrotscale", "bool", "Offer extra resolutions on a rotated head."),
    S("surround", "Display", "Surround (several displays as one)", "bootarg", "nvfbsurround", "int",
      "Bit mask of framebuffer indices that form one logical display, for example 3 for the first two. 0 or default turns it off.", rng=(1, 255)),
    S("edp-backlight", "Display", "Laptop panel brightness over the display link", "bootarg", "nvfbedpbl", "bool",
      "On by default; off disables brightness control through the panel's AUX channel."),
    S("fps-limit", "Performance", "Frame-rate limit", "conf", "NVMTL_FPS_LIMIT", "text",
      "One number for every application (60) or a list per application (Game=60,*=0). 10 to 1000; 0 is unlimited. Never applies to WindowServer.",
      effect="relaunch the application"),
    S("queue-depth", "Performance", "Low-latency mode (GPU work queued ahead)", "conf", "NVMTL_GSUB_INFLIGHT", "int",
      "How many parts of GPU work may be queued ahead, 1 to 32 (default 16). A low number cuts input lag and can cost frame rate. "
      "Applies to every application, the desktop included.", rng=(1, 32), effect="restart"),
    S("shader-cache-mb", "Performance", "Shader cache size limit (MB)", "conf", "NVMTL_SPVCACHE_MAX_MB", "int",
      "Size at which the shader cache is trimmed (default 2048). 0 turns trimming off.", rng=(64, 1000000), effect="relaunch the application"),
    S("multigpu", "Performance", "More than one NVIDIA card (experimental)", "bootarg", "nvmultigpu", "bool",
      "Accelerate every NVIDIA card, one start-up at a time. A process drives one card: the primary, or the one assigned below."),
    S("card", "Performance", "Card for an application", "conf", "NVMTL_CARD", "text",
      "With more than one card: 0 is the primary, 1 the second and so on (up to 7), for every application (1) or per application (Game=1,*=0).",
      effect="relaunch the application"),
    S("app-profiles", "Performance", "Built-in per-application profiles", "conf", "NVMTL_APP_PROFILE", "bool",
      "On by default; off turns every shipped per-application profile off.", effect="relaunch the application"),
    S("local-map-sysmem", "Performance", "Keep CPU-written memory in system RAM", "conf", "NVK_LOCAL_MAP_SYSMEM", "bool",
      "On by default for stability; off places it in video memory (faster in some games, unsafe for the desktop).", effect="restart"),
]
BY_ID = {s["id"]: s for s in CATALOG}

def normalize(s, value):
    """The text to store for `value`, or None for 'default' (remove). Raises ValueError with the reason."""
    v = value.strip()
    if v.lower() in ("default", "auto", "automatic", "unset"): return None
    k = s["kind"]
    if k == "bool":
        if v.lower() in ("on", "yes", "true", "1"): return "1"
        if v.lower() in ("off", "no", "false", "0"): return "0"
        raise ValueError("%s takes on, off or default, not %r" % (s["id"], value))
    if k == "enum":
        if v.lower() in s["values"]: return s["values"][v.lower()]
        if v in s["values"].values(): return v
        raise ValueError("%s takes one of %s or default, not %r" % (s["id"], ", ".join(s["values"]), value))
    if k == "int":
        zero_ok = s["id"] in ("surround", "vibrance", "shader-cache-mb")   # 0 means off / monochrome-free minimum / no trimming
        if not v.isdigit() or not (s["rng"][0] <= int(v) <= s["rng"][1]) and not (zero_ok and int(v) == 0):
            raise ValueError("%s takes a number from %d to %d, or default, not %r" % (s["id"], s["rng"][0], s["rng"][1], value))
        return str(int(v))
    if s["id"] == "card":
        for e in ([v] if "=" not in v else v.split(",")):
            num = e.split("=", 1)[1] if "=" in e else e
            if "=" in e and not e.split("=", 1)[0]: raise ValueError("card: an entry needs an application name or *")
            if not (len(num) == 1 and "0" <= num <= "7"): raise ValueError("card: %r is not a card number from 0 to 7" % num)
    if s["id"] == "fps-limit":
        entries = [v] if "=" not in v else v.split(",")
        for e in entries:
            num = e.split("=", 1)[1] if "=" in e else e
            if "=" in e and not e.split("=", 1)[0]: raise ValueError("fps-limit: an entry needs an application name or *")
            if not num.isdigit() or (int(num) != 0 and not 10 <= int(num) <= 1000):
                raise ValueError("fps-limit: %r is not 0 or a number from 10 to 1000" % num)
    if not KEPT.match(s["key"] + "=" + v): raise ValueError("%s: characters an update would not keep" % s["id"])
    return v

# ---- boot arguments ----
def bootargs_set(line, key, value):
    """Replace, add or (value None) remove one key=value in a boot-args line; the other arguments are returned untouched."""
    out, seen = [], False
    for tok in line.split():
        if tok.split("=", 1)[0] == key:
            if value is not None and not seen: out.append("%s=%s" % (key, value))
            seen = True
        else: out.append(tok)
    if value is not None and not seen: out.append("%s=%s" % (key, value))
    return " ".join(out)

def bootargs_get(line, key):
    for tok in line.split():
        if tok.split("=", 1)[0] == key: return tok.split("=", 1)[1] if "=" in tok else ""
    return None

# ---- conf user block ----
def conf_set(text, key, value):
    """The conf text with `key` set (or removed, value None) inside the user's block; the block is created when absent and
    removed again when it ends up empty. Lines outside the block are never touched."""
    lines = text.split("\n")
    if lines and lines[-1] == "": lines.pop()
    if BLOCK_START in lines and BLOCK_END in lines[lines.index(BLOCK_START):]:
        a = lines.index(BLOCK_START); b = lines.index(BLOCK_END, a)
        body = [l for l in lines[a + 1:b] if l.split("=", 1)[0] != key]
        if value is not None: body.append("%s=%s" % (key, value))
        lines[a:b + 1] = ([BLOCK_START] + body + [BLOCK_END]) if body else []
    elif value is not None:
        lines += [BLOCK_START, "%s=%s" % (key, value), BLOCK_END]
    return "\n".join(lines) + "\n"

def conf_get(text, key):
    inside = False; val = None
    for l in text.split("\n"):
        if l == BLOCK_START: inside = True
        elif l == BLOCK_END: inside = False
        elif inside and l.split("=", 1)[0] == key: val = l.split("=", 1)[1]
    return val

def plan(s, value, bootargs, conf):
    """What setting `s` to `value` does: ('bootarg', new line) or ('conf', new text). Raises ValueError for a bad value."""
    v = normalize(s, value)
    if s["where"] == "bootarg": return "bootarg", bootargs_set(bootargs, s["key"], v)
    return "conf", conf_set(conf, s["key"], v)

# ---- machine access (used only by the command line, not by the selftest) ----
def read_bootargs():
    r = subprocess.run(["/usr/sbin/nvram", "boot-args"], capture_output=True, text=True)
    return r.stdout.split("\t", 1)[1].strip() if r.returncode == 0 and "\t" in r.stdout else ""

def read_conf():
    try: return open(CONF).read()
    except OSError: return ""

def write_conf_atomic(text):
    d = os.path.dirname(CONF)
    fd, tmp = tempfile.mkstemp(dir=d, prefix=".nvrm610.")
    with os.fdopen(fd, "w") as f: f.write(text)
    os.chmod(tmp, 0o644); os.replace(tmp, CONF)

def current(s, bootargs, conf):
    v = bootargs_get(bootargs, s["key"]) if s["where"] == "bootarg" else conf_get(conf, s["key"])
    return "default" if v is None else v

def main(argv):
    if "--selftest" in argv: return selftest()
    if "--catalog-json" in argv: sys.stdout.write(catalog_json()); return 0
    if "--vectors-json" in argv: sys.stdout.write(vectors_json()); return 0
    if len(argv) < 2 or argv[1] not in ("list", "get", "set"): sys.exit(__doc__)
    ba, conf = read_bootargs(), read_conf()
    if argv[1] == "list":
        for s in CATALOG: print("%-17s %-12s %-9s %s\n%47s%s" % (s["id"], s["group"], current(s, ba, conf), s["title"], "", s["help"]))
        return 0
    if len(argv) < 3 or argv[2] not in BY_ID: sys.exit("unknown setting; run `list`")
    s = BY_ID[argv[2]]
    if argv[1] == "get": print(current(s, ba, conf)); return 0
    if len(argv) < 4: sys.exit("set needs a value")
    try: kind, new = plan(s, argv[3], ba, conf)
    except ValueError as e: print("refused: %s" % e, file=sys.stderr); return 2
    if "--apply" not in argv:
        print("would set %s (%s) to %s; nothing changed. Add --apply to write it." % (s["id"], s["key"], argv[3])); return 0
    if os.geteuid() != 0: print("--apply needs sudo", file=sys.stderr); return 3
    if kind == "bootarg":
        r = subprocess.run(["/usr/sbin/nvram", "boot-args=" + new]);
        if r.returncode: return r.returncode
    else: write_conf_atomic(new)
    print("set %s; takes effect after: %s" % (s["id"], s["effect"])); return 0

def catalog_json():
    """The catalogue as data, for the Mac app (mac/Resources/settings_catalog.json): one list, so the two cannot drift."""
    import json
    return json.dumps({"conf_path": CONF, "block_start": BLOCK_START, "block_end": BLOCK_END, "kept_pattern": KEPT.pattern,
                       "settings": CATALOG}, indent=1, sort_keys=True) + "\n"

def vectors_json():
    """Inputs with the answers THIS implementation gives, for the app's test: its Swift port must give the same ones."""
    import json
    cases = []
    vals = ["on", "off", "default", "auto", "1", "0", "maybe", "", " 60 ", "5", "10", "60", "1000", "1001", "-1", "3", "255", "256",
            "150", "200", "201", "4", "32", "33", "4096", "Game=60,*=0", "*=30", "=60", "Game=abc", "G=60;rm -rf /", "60 fps",
            "rgb", "ycbcr422", "ycbcr444", "full", "limited", "6", "8", "9", "12", "10bit", "x" * 200]
    for st in CATALOG:
        for v in vals:
            try: cases.append({"fn": "normalize", "id": st["id"], "value": v, "expect": normalize(st, v)})
            except ValueError: cases.append({"fn": "normalize", "id": st["id"], "value": v, "error": True})
    for line in ["", "-v keepsyms=1 nvfbhdr=1 debug=0x144", "nvfbhdr=1 x nvfbhdr=0", "nvfbhdrx=1", "nvfbvrr"]:
        for key, v in [("nvfbhdr", "1"), ("nvfbhdr", None), ("nvfbvrr", "0"), ("nvfbbpc", "10")]:
            cases.append({"fn": "bootargs_set", "line": line, "key": key, "value": v, "expect": bootargs_set(line, key, v)})
            cases.append({"fn": "bootargs_get", "line": line, "key": key, "expect": bootargs_get(line, key)})
    texts = ["", "NVMTL_A=1\nNVK_B=2\n", "NVMTL_A=1\n" + BLOCK_START + "\nNVMTL_FPS_LIMIT=60\nNVK_X=1\n" + BLOCK_END + "\nNVK_Z=3\n",
             BLOCK_START + "\n" + BLOCK_END + "\n", "NVMTL_FPS_LIMIT=30\n"]
    for t in texts:
        for key, v in [("NVMTL_FPS_LIMIT", "90"), ("NVMTL_FPS_LIMIT", None), ("NVK_X", None), ("NVMTL_GSUB_INFLIGHT", "4")]:
            cases.append({"fn": "conf_set", "text": t, "key": key, "value": v, "expect": conf_set(t, key, v)})
            cases.append({"fn": "conf_get", "text": t, "key": key, "expect": conf_get(t, key)})
    return json.dumps(cases, indent=0) + "\n"

def selftest():
    fails = []
    def arm(what, ok):
        print(("ok    " if ok else "FAIL  ") + what)
        if not ok: fails.append(what)
    def refuses(id, v):
        try: normalize(BY_ID[id], v); return False
        except ValueError: return True
    base = "-v keepsyms=1 nvfbhdr=1 debug=0x144"
    arm("setting a boot argument replaces only that argument", bootargs_set(base, "nvfbhdr", "0") == "-v keepsyms=1 nvfbhdr=0 debug=0x144")
    arm("a new boot argument is appended and the others stay in order", bootargs_set(base, "nvfbvrr", "1") == base + " nvfbvrr=1")
    arm("default removes the argument and leaves the rest", bootargs_set(base, "nvfbhdr", None) == "-v keepsyms=1 debug=0x144")
    arm("a duplicated argument collapses to one", bootargs_set("nvfbhdr=1 x nvfbhdr=0", "nvfbhdr", "1") == "nvfbhdr=1 x")
    arm("a key that only starts like ours is not touched", bootargs_set("nvfbhdrx=1", "nvfbhdr", "1") == "nvfbhdrx=1 nvfbhdr=1")
    arm("bool accepts on/off/default and refuses anything else", normalize(BY_ID["hdr"], "on") == "1" and normalize(BY_ID["hdr"], "off") == "0"
        and normalize(BY_ID["hdr"], "default") is None and refuses("hdr", "maybe"))
    arm("enum maps names to the kernel's numbers and refuses an unknown one", normalize(BY_ID["format"], "ycbcr444") == "2"
        and normalize(BY_ID["bpc"], "10") == "10" and refuses("bpc", "9") and refuses("format", "yuv"))
    arm("vibrance is 0 to 200 and refuses 201 or text", normalize(BY_ID["vibrance"], "150") == "150" and refuses("vibrance", "201") and refuses("vibrance", "x"))
    arm("surround takes a mask of 1 to 255 (0 means off)", normalize(BY_ID["surround"], "3") == "3" and refuses("surround", "256"))
    arm("fps-limit accepts a number and a per-application list", normalize(BY_ID["fps-limit"], "60") == "60"
        and normalize(BY_ID["fps-limit"], "Game=60,*=0") == "Game=60,*=0")
    arm("fps-limit refuses 5, 1001, an empty name, and characters an update would drop",
        refuses("fps-limit", "5") and refuses("fps-limit", "1001") and refuses("fps-limit", "=60") and refuses("fps-limit", "G=60;rm -rf /") and refuses("fps-limit", "60 fps"))
    conf = "NVMTL_A=1\nNVK_B=2\n"
    c1 = conf_set(conf, "NVMTL_FPS_LIMIT", "60")
    arm("a conf setting creates the user block and leaves the shipped lines alone", c1 == conf + BLOCK_START + "\nNVMTL_FPS_LIMIT=60\n" + BLOCK_END + "\n")
    c2 = conf_set(c1, "NVMTL_FPS_LIMIT", "90")
    arm("changing it again replaces the line inside the block", c2.count("NVMTL_FPS_LIMIT") == 1 and "NVMTL_FPS_LIMIT=90" in c2 and conf_get(c2, "NVMTL_FPS_LIMIT") == "90")
    arm("a shipped line with the same key outside the block is never edited", conf_set("NVMTL_FPS_LIMIT=30\n", "NVMTL_FPS_LIMIT", "60").startswith("NVMTL_FPS_LIMIT=30\n"))
    arm("removing the last setting removes the empty block", conf_set(c2, "NVMTL_FPS_LIMIT", None) == conf)
    arm("every line this tool writes passes the installer's keep rule",
        all(KEPT.match("%s=%s" % (s["key"], normalize(s, v))) for s, v in [(BY_ID["fps-limit"], "Game=60,*=0"), (BY_ID["app-profiles"], "off"), (BY_ID["local-map-sysmem"], "on")]))
    arm("queue depth is 1 to 32 and refuses 0 or 33 (0 would mean 'off' to nobody)", normalize(BY_ID["queue-depth"], "4") == "4" and refuses("queue-depth", "33") and refuses("queue-depth", "0"))
    arm("the shader cache limit takes megabytes, 0 turns trimming off, 10 is refused as too small", normalize(BY_ID["shader-cache-mb"], "4096") == "4096"
        and normalize(BY_ID["shader-cache-mb"], "0") == "0" and refuses("shader-cache-mb", "10"))
    arm("card takes 0 to 7 or a per-application list, and refuses 8, text and an empty name", normalize(BY_ID["card"], "1") == "1" and normalize(BY_ID["card"], "Game=1,*=0") == "Game=1,*=0"
        and refuses("card", "8") and refuses("card", "x") and refuses("card", "=1") and refuses("card", "Game=12"))
    arm("plan() routes boot arguments and conf settings to the right place", plan(BY_ID["hdr"], "on", "", "")[0] == "bootarg" and plan(BY_ID["fps-limit"], "60", "", "")[0] == "conf")
    arm("every catalogued id is unique and every boot-arg key is an nvfb name", len(BY_ID) == len(CATALOG) and all(s["key"].startswith(("nvfb", "nvmultigpu")) for s in CATALOG if s["where"] == "bootarg"))
    print("FAILED %d" % len(fails) if fails else "PASS all arms")
    return 1 if fails else 0

if __name__ == "__main__":
    sys.exit(main(sys.argv))
