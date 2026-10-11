# xesrc.py <linux tree> <amd.flags> <extra CONFIG...>: the sources Linux's drivers/gpu/drm/xe/Makefile builds into xe.o,
# evaluated against the shared CONFIG set (amd.flags) plus the xe-only options named on the command line. Handles the
# kbuild subset that Makefile uses: xe-y / xe-$(CONFIG_X) +=, backslash continuation, ifeq ($(CONFIG_X),y) / ifdef /
# ifneq / endif. Paths come out absolute; i915-display/ and i915-soc/ map to drivers/gpu/drm/i915/{display,soc} as the
# Makefile's pattern rules do. Sources under tests/ (KUnit) are never listed.
import re, sys, pathlib
tree = pathlib.Path(sys.argv[1])
kcfg = {l.strip()[len("-DCONFIG_"):].split("=")[0] for l in open(sys.argv[2]) if l.startswith("-DCONFIG_")}
# --i915 <dir>: where the shared i915 display/soc sources come from (the port uses drm-kmod's, which already build)
args = sys.argv[3:]
i915 = tree / "drivers/gpu/drm/i915"
if args[:1] == ["--i915"]:
    i915 = pathlib.Path(args[1]); args = args[2:]
kcfg |= set(args)
xe = tree / "drivers/gpu/drm/xe"
text = (xe / "Makefile").read_text()

def lines(t):
    buf = ""
    for raw in t.splitlines():
        line = raw.split("#", 1)[0]
        if line.rstrip().endswith("\\"):
            buf += line.rstrip()[:-1] + " "; continue
        yield buf + line; buf = ""

def on(name):
    return name in kcfg

stack, out = [True], []
for line in lines(text):
    s = line.strip()
    m = re.match(r"ifeq\s*\(\$\(CONFIG_([A-Z0-9_]+)\),\s*y\)", s)
    if m: stack.append(stack[-1] and on(m.group(1))); continue
    m = re.match(r"ifneq\s*\(\$\(CONFIG_([A-Z0-9_]+)\),\s*y\)", s)
    if m: stack.append(stack[-1] and not on(m.group(1))); continue
    m = re.match(r"ifdef\s+CONFIG_([A-Z0-9_]+)", s)
    if m: stack.append(stack[-1] and on(m.group(1))); continue
    if s.startswith("else"): prev = stack.pop(); stack.append(stack[-1] and not prev); continue
    if s.startswith("endif"): stack.pop(); continue
    if not all(stack): continue
    m = re.match(r"xe-(y|\$\(CONFIG_([A-Z0-9_]+)\))\s*\+?=\s*(.*)", s)
    if not m: continue
    if m.group(1) != "y" and not on(m.group(2)): continue
    for w in m.group(3).split():
        if not w.endswith(".o") or w.startswith("tests/"): continue
        c = w[:-2] + ".c"
        if c.startswith("i915-display/"): p = i915 / "display" / c[len("i915-display/"):]
        elif c.startswith("i915-soc/"): p = i915 / "soc" / c[len("i915-soc/"):]
        else: p = xe / c
        out.append(str(p))
seen = set()
for p in out:
    if p not in seen:
        seen.add(p); print(p)
