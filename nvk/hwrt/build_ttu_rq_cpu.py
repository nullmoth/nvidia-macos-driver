#!/usr/bin/env python3
"""Build ttu_rq_cpu (x86_64, runs under Rosetta) against a patched NVK build tree: the test, the ray-query lowering and the
shared traversal helpers it calls, linked with the tree's NAK and NIR static libraries.
usage: build_ttu_rq_cpu.py <tree> <builddir> <ttu_rq_cpu.c> <out-binary>"""
import json, shlex, subprocess, sys
from pathlib import Path

tree, bdir, test, out = (Path(a) for a in sys.argv[1:5])
b = tree / bdir
cmds = json.loads((b / 'compile_commands.json').read_text())


def compile_one(match, src, obj):
    c = next(x for x in cmds if x['file'].endswith(match))
    args = shlex.split(c['command'])
    args[args.index('-o') + 1] = str(obj)
    for flag in ('-MF', '-MQ'):
        if flag in args:
            i = args.index(flag)
            del args[i:i + 2]
    if '-MD' in args:
        args.remove('-MD')
    args[-1] = str(src)
    r = subprocess.run(args, cwd=b, capture_output=True, text=True)
    if r.returncode:
        sys.exit('compile failed for %s:\n%s' % (src, r.stderr[-4000:]))
    return args[0]


objs = []
cc = compile_one('/nak_nir.c', test, out.with_suffix('.o'))
objs.append(out.with_suffix('.o'))
for name in ('nvk_nir_lower_ray_queries.c', 'nvk_nir_ray_tracing.c'):
    src = tree / 'src/nouveau/vulkan' / name
    o = out.with_suffix('.' + name + '.o')
    compile_one('/' + name, src, o)
    objs.append(o)

libs = [str(p) for p in sorted(b.rglob('*.a'))
        if any(k in p.name for k in ('nak', 'nir', 'compiler', 'util', 'blake3', 'c11', 'nvidia_headers', 'vtn'))]
link = [cc, '-arch', 'x86_64', '-mmacosx-version-min=15.0', *map(str, objs), *libs, *libs, '-lc++', '-lz', '-lm', '-o', str(out)]
r = subprocess.run(link, cwd=b, capture_output=True, text=True)
if r.returncode:
    sys.exit('link failed:\n' + r.stderr[-6000:])
print('built', out)
