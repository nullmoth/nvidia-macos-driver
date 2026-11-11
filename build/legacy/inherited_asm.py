#!/usr/bin/env python3
# inherited_asm.py <accelerator family binary> <our linked kext binary> <out.s>
# Releases whose kernel links kexts itself (before 11): a method our classes inherit must either be exported by
# the family or be defined in our kext. Apple's private headers define a number of small methods inline, so the
# family keeps them as local symbols and every vendor driver carries its own copy. This writes our copies: for
# each undefined name of ours that the family defines only locally, the family's own instructions, verbatim.
# Only bodies made of register moves, field loads and stores, constants, an indirect tail jump and a return are
# taken; anything else is reported and the exit status is 1, so nothing is guessed.
import re, subprocess, sys
fam, ours, out = sys.argv[1:4]
def nm(args, p): return subprocess.run(["nm", "-arch", "x86_64"] + args + [p], capture_output=True, text=True, errors="replace").stdout.splitlines()
exported = {l.split()[-1] for l in nm(["-gU"], fam) if l.strip()}
local = {l.split()[-1] for l in nm([], fam) if len(l.split()) == 3 and l.split()[1] in "tT"}
want = [n for n in (l.split()[-1] for l in nm(["-u"], ours) if l.strip()) if n.startswith("__ZN") and n in local and n not in exported]
dis = {}; cur = None
for l in subprocess.run(["otool", "-arch", "x86_64", "-tV", fam], capture_output=True, text=True, errors="replace").stdout.splitlines():
    if l.endswith(":") and not l.startswith("\t") and not l[0].isdigit():
        cur = l[:-1]; cur = None if cur in dis else cur        # the first copy of a name is the one kept
        if cur: dis[cur] = []
        continue
    if cur is not None and "\t" in l: dis[cur].append(l.split("\t", 1)[1].split("##")[0].strip())
REG = r"%(?:[re]?[abcd]x|[re]?[sd]il?|[abcd]l|r(?:8|9|1[0-5])[dwb]?)"
OK = [rf"mov[lqbw]\s+(?:0x[0-9a-f]+)?\(%r[a-z0-9]+\), {REG}$", rf"mov[lqbw]\s+{REG}, 0x[0-9a-f]+\(%r[a-z0-9]+\)$",
      rf"mov[lqbw]\s+\$0x[0-9a-f]+, {REG}$", rf"mov(?:[lq]|zb[lq]|zw[lq])\s+{REG}, {REG}$", rf"xorl\s+{REG}, {REG}$",
      r"jmpq\s+\*(?:0x[0-9a-f]+\(%rax\)|%rax)$", r"retq$", r"pushq\s+%rbp$", r"movq\s+%rsp, %rbp$", r"popq\s+%rbp$"]
lines = ["# Copies of methods Apple's headers define inline (bodies read from this release's accelerator family).", "\t.text"]; bad = []
for n in want:
    body = [x for x in dis.get(n, []) if not x.startswith("nop")]
    if not body or not all(any(re.match(p, x) for p in OK) for x in body) or not re.match(r"(retq|jmpq)", body[-1]):
        bad.append((n, "; ".join(body)[:160])); continue
    lines += ["\t.globl\t" + n, "\t.p2align\t4, 0x90", n + ":"] + ["\t" + x for x in body]
open(out, "w").write("\n".join(lines) + "\n")
print(f"inherited inline methods carried: {len(want) - len(bad)} of {len(want)}")
for n, b in bad: print(f"  NOT TAKEN {n}: {b or 'no body found'}")
sys.exit(1 if bad else 0)
