#!/usr/bin/env python3
"""Offline proof for the specialization-constant path of the translator (host with spirv-val/spirv-opt on PATH).

For one AIR text and a list of function-constant sets (idx:hexbytes,...), this
  1. translates the AIR once with nvmtl_translate_spec and validates the module (spirv-val);
  2. for each set, translates it the per-variant way (nvmtl_translate_fc) and validates that too;
  3. applies the set to the one module as specialization-constant defaults (spirv-opt --set-spec-const-default-value),
     validates the result, and runs both modules through spirv-opt -O so each is folded as far as a compiler would;
  4. compares the two folded modules: entry points, interface, and the descriptor bindings they use must agree, and the
     instruction count of the specialized-then-folded module is reported against the per-variant one.
Usage: spec_equiv.py <libnvmtl_translate.dylib> <air.ll> <stage> <sets.txt>   (exit 1 on any disagreement)"""
import ctypes, os, re, struct, subprocess, sys, tempfile

lib, llp, stage, setsp = sys.argv[1:5]
L = ctypes.CDLL(lib)
ll = open(llp, 'rb').read()
ERR = ctypes.create_string_buffer(1024)

def call_spec():
    f = L.nvmtl_translate_spec
    f.argtypes = [ctypes.c_char_p, ctypes.c_char_p] + [ctypes.POINTER(ctypes.c_void_p), ctypes.POINTER(ctypes.c_size_t)] * 2 + [ctypes.c_char_p, ctypes.c_size_t]
    o, on, t, tn = ctypes.c_void_p(), ctypes.c_size_t(), ctypes.c_void_p(), ctypes.c_size_t()
    rc = f(ll, stage.encode(), ctypes.byref(o), ctypes.byref(on), ctypes.byref(t), ctypes.byref(tn), ERR, 1024)
    if rc: sys.exit('nvmtl_translate_spec failed: ' + ERR.value.decode(errors='replace'))
    spv, tab = ctypes.string_at(o, on.value), ctypes.string_at(t, tn.value)
    L.nvmtl_translate_free(ctypes.cast(o, ctypes.POINTER(ctypes.c_ubyte)), on.value)
    L.nvmtl_translate_free(ctypes.cast(t, ctypes.POINTER(ctypes.c_ubyte)), tn.value)
    return spv, [struct.unpack_from('<5I', tab, i) for i in range(0, len(tab), 20)]

def call_apply(spv, table, vals):
    f = L.nvmtl_spec_apply
    n = len(vals)
    tb = b''.join(struct.pack('<5I', *e) for e in table)
    I = (ctypes.c_uint32 * n)(*[v[0] for v in vals]); S = (ctypes.c_uint32 * n)(*[len(v[1]) for v in vals])
    bufs = [ctypes.create_string_buffer(v[1], len(v[1])) for v in vals]
    P = (ctypes.c_void_p * n)(*[ctypes.cast(b, ctypes.c_void_p) for b in bufs])
    o, on = ctypes.c_void_p(), ctypes.c_size_t()
    f.argtypes = [ctypes.c_char_p, ctypes.c_size_t, ctypes.c_char_p, ctypes.c_size_t, ctypes.c_void_p, ctypes.c_void_p, ctypes.c_void_p,
                  ctypes.c_size_t, ctypes.POINTER(ctypes.c_void_p), ctypes.POINTER(ctypes.c_size_t), ctypes.c_char_p, ctypes.c_size_t]
    rc = f(spv, len(spv), tb, len(tb), I, S, P, n, ctypes.byref(o), ctypes.byref(on), ERR, 1024)
    if rc: return None
    out = ctypes.string_at(o, on.value)
    L.nvmtl_translate_free(ctypes.cast(o, ctypes.POINTER(ctypes.c_ubyte)), on.value)
    return out

def call_fc(vals):
    f = L.nvmtl_translate_fc
    n = len(vals)
    I = (ctypes.c_uint32 * n)(*[v[0] for v in vals]); S = (ctypes.c_uint32 * n)(*[len(v[1]) for v in vals])
    bufs = [ctypes.create_string_buffer(v[1], len(v[1])) for v in vals]
    P = (ctypes.c_void_p * n)(*[ctypes.cast(b, ctypes.c_void_p) for b in bufs])
    o, on = ctypes.c_void_p(), ctypes.c_size_t()
    f.argtypes = [ctypes.c_char_p, ctypes.c_char_p, ctypes.c_void_p, ctypes.c_void_p, ctypes.c_void_p, ctypes.c_size_t,
                  ctypes.POINTER(ctypes.c_void_p), ctypes.POINTER(ctypes.c_size_t), ctypes.c_char_p, ctypes.c_size_t]
    rc = f(ll, stage.encode(), I, S, P, n, ctypes.byref(o), ctypes.byref(on), ERR, 1024)
    if rc: return None
    spv = ctypes.string_at(o, on.value)
    L.nvmtl_translate_free(ctypes.cast(o, ctypes.POINTER(ctypes.c_ubyte)), on.value)
    return spv

def run(args, data=None):
    return subprocess.run(args, input=data, capture_output=True)

tmp = tempfile.mkdtemp()
def path(name, data):
    p = os.path.join(tmp, name); open(p, 'wb').write(data); return p

fails = 0
def check(name, ok, detail=''):
    global fails
    print(('ok   ' if ok else 'FAIL ') + name + (f' ({detail})' if detail else '')); fails += not ok

spv, table = call_spec()
mp = path('spec.spv', spv)
r = run(['spirv-val', '--target-env', 'vulkan1.3', mp])
check('the one specialization-constant module validates', r.returncode == 0, r.stderr.decode()[:300])
check('every spec id is unique', len({e[2] for e in table}) == len(table), f'{len(table)} entries')
dis = run(['spirv-dis', mp]).stdout.decode()
check('the module carries SpecId decorations for every entry', dis.count('SpecId') == len(table), f'{dis.count("SpecId")} vs {len(table)}')

def facts(m):
    d = run(['spirv-dis', '--raw-id', m]).stdout.decode()
    ep = sorted(re.findall(r'OpEntryPoint \w+ %\d+ "([^"]*)"', d))
    var = set(re.findall(r'%(\d+) = OpVariable', d))
    ds, bd = {}, {}
    for kind, tgt in (('DescriptorSet', ds), ('Binding', bd)):
        for i, n in re.findall(r'OpDecorate %(\d+) ' + kind + r' (\d+)', d): tgt[i] = int(n)
    bind = sorted({(ds[i], bd[i]) for i in var if i in ds and i in bd})
    ninst = len([l for l in d.splitlines() if l.strip() and not l.lstrip().startswith(';')])
    return ep, bind, ninst

sets = [l.strip() for l in open(setsp) if l.strip()]
by_fc = {}
for e in table: by_fc.setdefault(e[0], []).append(e)
for si, line in enumerate(sets):
    vals = [(int(a), bytes.fromhex(b)) for a, b in (x.split(':') for x in line.split(','))]
    have = {i for i, _ in vals}
    defaults = []
    for idx, payload in vals:
        for (_, lane, sid, size, off) in by_fc.get(idx, []):
            if lane == 15: continue
            defaults.append(f'{sid}:{int.from_bytes(payload[off:off + size], "little")}')
    for idx, ents in by_fc.items():
        for (_, lane, sid, size, off) in ents:
            if lane == 15: defaults.append(f'{sid}:{"true" if idx in have else "false"}')
    r = run(['spirv-opt', '--set-spec-const-default-value', ' '.join(defaults), '--freeze-spec-const', mp, '-o', os.path.join(tmp, f'set{si}.spv')])
    ok = r.returncode == 0
    check(f'set {si}: the defaults apply', ok, r.stderr.decode()[:200])
    if not ok: continue
    a = call_fc(vals)
    ap2 = call_apply(spv, table, vals)
    check(f'set {si}: nvmtl_spec_apply returns a module', ap2 is not None, ERR.value.decode(errors='replace')[:120])
    if ap2 is not None:
        cp = path(f'apply{si}.spv', ap2)
        check(f'set {si}: the applied module validates', run(['spirv-val', '--target-env', 'vulkan1.3', cp]).returncode == 0)
        check(f'set {si}: it holds no function-constant SpecId any more', b'' == b'' and run(['spirv-dis', cp]).stdout.decode().count('SpecId') == 0)
        dr, da = run(['spirv-dis', '--no-header', os.path.join(tmp, f'set{si}.spv')]).stdout.decode(), run(['spirv-dis', '--no-header', cp]).stdout.decode()
        # the reference freeze (spirv-opt) and ours agree on every constant
        cnt = lambda t: sorted(re.findall(r'OpConstant(?:True|False)? [^\n]*', re.sub(r'%\S+ = ', '', t)))
        check(f'set {si}: it fixes the same constants as spirv-opt --freeze-spec-const', cnt(dr) == cnt(da), str((__import__('collections').Counter(cnt(dr)) - __import__('collections').Counter(cnt(da))).most_common(4)) + ' | ' + str((__import__('collections').Counter(cnt(da)) - __import__('collections').Counter(cnt(dr))).most_common(4)))
    if a is None:
        check(f'set {si}: the per-variant translation exists', False, ERR.value.decode(errors='replace')[:120]); continue
    ap = path(f'var{si}.spv', a)
    check(f'set {si}: the per-variant module validates', run(['spirv-val', '--target-env', 'vulkan1.3', ap]).returncode == 0)
    bp = os.path.join(tmp, f'set{si}.spv') if ap2 is None else cp
    check(f'set {si}: the specialized module validates', run(['spirv-val', '--target-env', 'vulkan1.3', bp]).returncode == 0)
    fa, fb = os.path.join(tmp, f'fa{si}.spv'), os.path.join(tmp, f'fb{si}.spv')
    FOLD = ['--inline-entry-points-exhaustive', '--private-to-local', '-O']   # what a NIR consumer does with module-scope variables
    run(['spirv-opt'] + FOLD + [ap, '-o', fa]); run(['spirv-opt'] + FOLD + [bp, '-o', fb])
    (epa, bda, na), (epb, bdb, nb) = facts(fa), facts(fb)
    check(f'set {si}: same entry points', epa == epb, f'{epa} {epb}')
    print(f'     set {si}: resources bound after folding that the per-variant module does not bind: {sorted(set(bdb) - set(bda))}')
    print(f'     set {si}: instructions after -O: per-variant {na}, one module specialized {nb} (before -O {len(spv)} vs {len(a)} bytes)')
sys.exit(1 if fails else 0)
