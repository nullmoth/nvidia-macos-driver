#!/usr/bin/env python3
"""Build ttu_nir_cpu (x86_64, runs under Rosetta) against a candidate NVK build tree's static libraries.
usage: build_ttu_cpu.py <tree> <builddir> <source.c> <out-binary>"""
import json, shlex, subprocess, sys
from pathlib import Path

tree, bdir, src, out = (Path(a) for a in sys.argv[1:5])
b = tree / bdir
cmds = json.loads((b / 'compile_commands.json').read_text())
c = next(x for x in cmds if x['file'].endswith('/nak_nir.c'))
args = shlex.split(c['command'])
o = out.with_suffix('.o')
args[args.index('-o') + 1] = str(o)
for flag in ('-MF', '-MQ'):
    if flag in args:
        i = args.index(flag)
        del args[i:i + 2]
if '-MD' in args:
    args.remove('-MD')
args[-1] = str(src)
r = subprocess.run(args, cwd=b, capture_output=True, text=True)
if r.returncode:
    sys.exit('compile failed:\n' + r.stderr[-4000:])

# Every static library NAK/NIR needs; order matters for the archive linker, so list it twice.
libs = [str(p) for p in sorted(b.rglob('*.a'))
        if any(k in p.name for k in ('nak', 'nir', 'compiler', 'util', 'blake3', 'c11', 'nvidia_headers', 'vtn'))]
link = [args[0], *[a for a in args[1:3] if a.startswith('-arch') or a == 'x86_64'], '-arch', 'x86_64',
        '-mmacosx-version-min=15.0', str(o), *libs, *libs, '-lc++', '-lz', '-lm', '-o', str(out)]
r = subprocess.run(link, cwd=b, capture_output=True, text=True)
if r.returncode:
    sys.exit('link failed:\n' + r.stderr[-6000:])
print('built', out)
