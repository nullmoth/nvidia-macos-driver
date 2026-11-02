#!/usr/bin/env python3
"""spec-tables.py: extract the default probability tables from a codec specification's text (pdftotext -layout) into
a C header, checking every table's number count against its declared dimensions.
  scripts/spec-tables.py vp9 vendor/specs/vp9-spec.txt src/vp9tables.h
  scripts/spec-tables.py av1 vendor/specs/av1-spec.txt src/av1tables.h   (constants read from the spec's own table)
"""
import re, sys
VP9 = dict(INTRA_MODES=10, PARTITION_CONTEXTS=16, PARTITION_TYPES=4, BLOCK_SIZE_GROUPS=4, SKIP_CONTEXTS=3,
           IS_INTER_CONTEXTS=4, COMP_MODE_CONTEXTS=5, REF_CONTEXTS=5, MV_OFFSET_BITS=10, TX_SIZES=4, TX_SIZE_CONTEXTS=2,
           INTER_MODE_CONTEXTS=7, INTER_MODES=4, INTERP_FILTER_CONTEXTS=4, SWITCHABLE_FILTERS=3, MV_JOINTS=4,
           MV_CLASSES=11, CLASS0_SIZE=2, BLOCK_TYPES=2, REF_TYPES=2, COEF_BANDS=6, PREV_COEF_CONTEXTS=6,
           UNCONSTRAINED_NODES=3)

def spec_consts(text):
    # "NAME   value   description" rows of the spec's constants section (section 3)
    c = {}
    for m in re.finditer(r"^\s*([A-Z][A-Z0-9_]{2,})\s+(-?\d+)\s+\S", text, re.M): c.setdefault(m.group(1), int(m.group(2)))
    return c

NAMES = r"(?:(?:kf|default|Default)_[A-Za-z0-9_]+|Gaussian_Sequence)"

def clean(text):
    # page furniture (footer + header at each page break), /* */ comments, form feeds
    text = re.sub(r"/\*.*?\*/", " ", text.replace("\f", "\n"), flags=re.S)
    keep = [l for l in text.split("\n") if "Copyright \u00a9" not in l and "Bitstream & Decoding Process Specification" not in l
            and "AV1 Bitstream" not in l and not re.match(r"^Section: .* Page \d+ of \d+", l)]
    # a declaration whose dimensions continue on the next line(s): join them
    out = []
    for l in keep:
        if out and re.match(r"^\s*\[", l) and re.search(NAMES + r"\s*(\[[^\]]*\]\s*)+$", out[-1]): out[-1] += " " + l.strip()
        elif out and re.search(NAMES, out[-1]) and out[-1].count("[") > out[-1].count("]"): out[-1] += " " + l.strip()   # inside a bracket
        else: out.append(l)
    return out

def tables(text, consts):
    lines = clean(text)
    out = []; i = 0
    decl = re.compile(r"^\s*(" + NAMES + r")\s*((?:\[[^\]]*\]\s*)+)(=\s*\{.*)?\s*$")
    while i < len(lines):
        m = decl.match(lines[i])
        if not m: i += 1; continue
        name, dims = m.group(1), re.findall(r"\[([^\]]*)\]", m.group(2))
        # the body: from the first '{' to its matching '}'
        body = []; depth = 0; started = False; j = i
        while j < len(lines):
            l = lines[j].split("//")[0]
            if j == i: l = l[l.find("=") + 1:] if "=" in l else ""
            for ch in l:
                if ch == "{": depth += 1; started = True
                elif ch == "}": depth -= 1
            body.append(l)
            if started and depth == 0: break
            j += 1
        flat = re.sub(r"(\d+)\s*\*\s*(\d+)", lambda m: str(int(m.group(1)) * int(m.group(2))), " ".join(body))   # AV1: 128*125
        nums = [int(x) for x in re.findall(r"(?<![A-Za-z_\d])-?\d+(?![\d])", flat)]
        try: shape = [eval(d, {}, consts) for d in dims]
        except Exception: shape = None
        out.append((name, dims, shape, nums)); i = j + 1
    return out

def main(codec, src, dst):
    text = open(src, errors="replace").read()
    consts = VP9 if codec == "vp9" else dict(spec_consts(text), RESTORE_SWITCHABLE=3)   # an enum value, not in the table
    t = tables(text, consts)
    o = [f"// {dst.split('/')[-1]}: default probability tables from the {codec.upper()} specification (pdftotext of the",
         "// published spec, extracted by scripts/spec-tables.py; counts checked against the declared dimensions).",
         "#pragma once", "#include <stdint.h>", "",
         "// flat initializers (the spec's tables as printed)", "#pragma GCC diagnostic push",
         "#pragma GCC diagnostic ignored \"-Wmissing-braces\"", ""]
    for name, dims, shape, nums in t:
        want = 1
        for s in (shape or [0]): want *= s
        ok = shape is not None and want == len(nums)
        print(f"{name:32s} dims {dims} -> {shape} want {want} got {len(nums)} {'ok' if ok else 'CHECK'}")
        ty = "int16_t" if min(nums, default=0) < 0 else "uint8_t" if max(nums, default=0) <= 255 else "uint16_t"
        if ok:
            o.append(f"static const {ty} {name}{''.join(f'[{s}]' for s in shape)} = {{")
            for k in range(0, len(nums), 24): o.append("    " + ", ".join(map(str, nums[k:k + 24])) + ",")
            o.append("};")
        else:
            o.append(f"// {name}: {len(nums)} values for dims {dims} (jagged; laid out by hand below)")
            o.append(f"static const {ty} {name}_flat[{len(nums)}] = {{")
            for k in range(0, len(nums), 24): o.append("    " + ", ".join(map(str, nums[k:k + 24])) + ",")
            o.append("};")
    o.append("#pragma GCC diagnostic pop")
    open(dst, "w").write("\n".join(o) + "\n")

if __name__ == "__main__":
    main(*sys.argv[1:4])
