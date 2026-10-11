# srclist.py <drm-kmod> <amd.flags> <module...>: the sources drm-kmod's Makefile for each module builds, evaluated
# against the same KCONFIG set as the compile flags. Handles the BSD make subset these Makefiles use: SRCS=/+=,
# .PATH:, .if/.elif !empty(KCONFIG:M...) / empty(...), .else, .endif, '#' comments, backslash continuation.
import re, sys, pathlib
dk = pathlib.Path(sys.argv[1])
kcfg = {l.strip()[len("-DCONFIG_"):].split("=")[0] for l in open(sys.argv[2]) if l.startswith("-DCONFIG_")}
def logical_lines(text):
    buf = ""
    for raw in text.splitlines():
        line = raw.split("#", 1)[0] if not raw.lstrip().startswith(".") else re.sub(r"\s#.*$", "", raw)
        if line.rstrip().endswith("\\"):
            buf += line.rstrip()[:-1] + " "; continue
        yield buf + line; buf = ""
def cond(expr):
    expr = expr.strip()
    out = expr
    # a pattern may end in '*' (BSD make :M glob): it matches any option with that prefix
    for neg, name in re.findall(r"(!?)empty\(KCONFIG:M([A-Z0-9_]+\*?)\)", expr):
        if name.endswith("*"):
            present = any(k.startswith(name[:-1]) for k in kcfg)
        else:
            present = name in kcfg
        val = present if neg == "!" else not present
        out = out.replace(f"{neg}empty(KCONFIG:M{name})", "True" if val else "False", 1)
    out = re.sub(r'\$\{MACHINE_CPUARCH\}\s*==\s*"amd64"', "True", out)
    out = re.sub(r'\$\{MACHINE_CPUARCH\}\s*==\s*"[a-z0-9]+"', "False", out)
    out = re.sub(r'\$\{MACHINE_ARCH:M[^}]*\}\s*!=\s*""', "False", out)
    out = out.replace("||", " or ").replace("&&", " and ").replace("!", " not ")
    try: return bool(eval(out, {}, {}))
    except Exception: return False
for mod in sys.argv[3:]:
    mk = dk / mod / "Makefile"
    text = mk.read_text()
    srcs, paths, stack = [], [], [True]
    mkvars = {}
    for line in logical_lines(text):
        s = line.strip()
        if s.startswith(".if"):
            stack.append(stack[-1] and cond(s[3:]))
        elif s.startswith(".elif"):
            prev = stack.pop(); stack.append(stack[-1] and not prev and cond(s[5:]))
        elif s.startswith(".else"):
            prev = stack.pop(); stack.append(stack[-1] and not prev)
        elif s.startswith(".endif"):
            stack.pop()
        elif not all(stack):
            continue
        elif s.startswith(".PATH:"):
            # lowercase make variables (e.g. _dml, the display math directories) expand to their words
            body = re.sub(r"\$\{(_?[a-z][a-z0-9_]*)\}", lambda m: " ".join(mkvars.get(m.group(1), [])), s[6:])
            for t in body.split():
                t = t.replace("${SRCDIR}", "SRCDIR").replace("${DRM}", "DRM")
                paths.append(t)
        elif re.match(r"_?[a-z][a-z0-9_]*\s*\+?=", s):
            name, _, val = re.split(r"\s*(\+?=)\s*", s, maxsplit=1)
            words = val.split()
            mkvars[name] = (mkvars.get(name, []) + words) if s.split("=", 1)[0].endswith("+") else words
        elif re.match(r"SRCS\s*\+?=", s):
            srcs += [w for w in re.split(r"\s+", s.split("=", 1)[1]) if w.endswith(".c")]
    curdir = mk.parent
    def expand(v):
        # ${.CURDIR}, ${.CURDIR:H}, ${.CURDIR:H:H}: BSD make's :H is the parent directory
        def rep(m):
            d = curdir
            for _ in range(m.group(1).count(":H")): d = d.parent
            return str(d)
        return re.sub(r"\$\{\.CURDIR((?::H)*)\}", rep, v)
    srcdir_m = re.search(r"^SRCDIR\s*=\s*(\S+)", text, re.M)
    srcdir = pathlib.Path(expand(srcdir_m.group(1))) if srcdir_m else curdir
    drm = dk / "drivers/gpu/drm"
    cand = [pathlib.Path(expand(p.replace("SRCDIR", str(srcdir)).replace("DRM", str(drm)))) for p in paths] + [srcdir]
    for s in dict.fromkeys(srcs):
        hit = next((c / s for c in cand if (c / s).exists()), None)
        if hit is None: print("#MISSING", mod, s, file=sys.stderr); continue
        print(hit)
