#!/usr/bin/env python3
"""trace_diff.py A.txt B.txt [--frames N] [--context K]

Pass-by-pass comparison of two per-draw driver traces (/tmp/nvmtl-trace-<pid>.txt) of the same scene on two plugin
builds. Each trace is cut into command buffers ("CB commit #n") and each command buffer into events: render passes
(PASS ... + their DRAW lines with the fragment textures and samplers bound at that draw), compute dispatches
(DISPATCH ...), blits (BLIT ...), present copies (SURFOUT ...). Pointers, command-buffer serials, surface ids and
other per-process values are normalized away so that two builds whose trace vocabularies differ slightly (one prints
"DRAW cb %p vk %p type", the other "DRAW type") still compare event for event. The first command buffer in which the
event sequences diverge is printed with the surrounding events of both traces; that pass names the mechanism.

The comparison starts at the first complete command buffer of each trace (a trace request lands mid-frame) and runs
over at most --frames command buffers. --selftest builds two synthetic traces (one in each vocabulary) of the same
frame, expects 0 divergences, then alters one bound texture in a later pass of the second and expects the divergence to
be reported at exactly that pass (the negative control that must fire)."""
import re, sys

# One regex per event kind, applied to the line AFTER normalization (see normalize()).
EVENT = [
    ('CB',       re.compile(r'^CB commit #(\d+)')),
    ('PASS',     re.compile(r'^PASS target (\d+x\d+) surf \S+ view (-?\d+) load (\d+) store (\d+) clear \((.*?)\) depth (\d+)')),
    ('DRAW',     re.compile(r'^DRAW (?:patches \d+ cp|type \d+) \| vs (\S+) \| fs (\S+) \| blend (.*)$')),
    ('PSO',      re.compile(r'^PSO \S+ (\S+) / (\S+) vs ([0-9a-f]{8}) fs ([0-9a-f]{8})')),
    ('DISPATCH', re.compile(r'^DISPATCH (\S+) \| threads (\d+x\d+x\d+) tg (\d+x\d+x\d+)')),
    ('FTEX',     re.compile(r'^FTEX\[(\d+)\] (\d+x\d+) surf \S+ view (-?\d+)')),
    ('FSAMP',    re.compile(r'^FRAGMENT SAMPLER\[(\d+)\] pixel=(\d) bound=(\d)')),
    ('BLIT',     re.compile(r'^BLIT (tex \d+x\d+|buf) .*?-> (tex \d+x\d+|buf)')),
    ('SURFOUT',  re.compile(r'^SURFOUT surf \S+ (\d+x\d+)')),
]

def normalize(line):
    """Strip the per-process noise so both vocabularies read the same: pointers, the dev build's "cb %p vk %p" and
    "cb %p" insertions, and the command-buffer serial (kept separately by the CB regex)."""
    s = line.rstrip('\n')
    s = re.sub(r'\bcb 0x[0-9a-f]+ vk 0x[0-9a-f]+ ', '', s)
    s = re.sub(r'\bcb 0x[0-9a-f]+ ', '', s)
    s = re.sub(r'\b0x[0-9a-fA-F]+', '0x', s)   # a word boundary, or 1920x1080 would lose its height
    return s

def parse(lines):
    """-> list of command buffers; each a list of (kind, key) events. A render pass is one event whose key folds the
    pass geometry and every DRAW (with its FTEX/FSAMP state) inside it, so a pass compares as a unit."""
    lines = list(lines)
    has_cb = any(l.startswith('CB commit') for l in lines)
    cbs, cur, pas, draw_state = [], None, None, []
    def close_pass():
        nonlocal pas
        if pas is not None and cur is not None:
            cur.append(('PASS', pas['head'] + ' | ' + ' ; '.join(pas['draws'])))
        pas = None
    for raw in lines:
        s = normalize(raw)
        kind = m = None
        for k, rx in EVENT:
            m = rx.match(s)
            if m: kind = k; break
        if not kind: continue
        if kind == 'CB' or (kind == 'SURFOUT' and not has_cb):
            # a trace without "CB commit" lines (the 2.7 vocabulary) is cut at the present instead: one unit per frame
            close_pass()
            cur = []; cbs.append(cur); draw_state = []
            if kind == 'SURFOUT': continue
        elif cur is None:
            continue                      # before the first complete command buffer
        elif kind == 'PASS':
            close_pass()
            pas = {'head': 'target %s view %s load %s store %s clear (%s) depth %s' % m.groups(), 'draws': []}
            draw_state = []
        elif kind in ('FTEX', 'FSAMP'):
            draw_state.append(kind + ' ' + ' '.join(m.groups()))
        elif kind == 'PSO':
            draw_state.append('PSO %s/%s %s %s' % m.groups())
        elif kind == 'DRAW':
            d = 'DRAW vs %s fs %s blend %s [%s]' % (m.group(1), m.group(2), m.group(3), ', '.join(draw_state))
            draw_state = []
            if pas is not None: pas['draws'].append(d)
            else: cur.append(('DRAW', d))
        else:
            close_pass()
            cur.append((kind, ' '.join(g for g in m.groups() if g is not None)))
    close_pass()
    return cbs

def diff(a, b, frames=None, context=3):
    """-> (n_cbs_compared, n_equal_cbs, first_divergence or None). The first divergence is a dict with the command
    buffer index, event index and the events of both sides around it."""
    n = min(len(a), len(b))
    if frames: n = min(n, frames)
    equal = 0
    for i in range(n):
        ea, eb = a[i], b[i]
        if ea == eb: equal += 1; continue
        j = 0
        while j < len(ea) and j < len(eb) and ea[j] == eb[j]: j += 1
        lo = max(0, j - context)
        return n, equal, {'cb': i, 'event': j, 'a': ea[lo:j + context + 1], 'b': eb[lo:j + context + 1],
                          'a_len': len(ea), 'b_len': len(eb)}
    return n, equal, None

def report(a_cbs, b_cbs, frames, context):
    n, equal, first = diff(a_cbs, b_cbs, frames, context)
    print('command buffers: A %d, B %d, compared %d, identical %d' % (len(a_cbs), len(b_cbs), n, equal))
    if not first:
        print('no divergence in the compared command buffers'); return 0
    print('FIRST DIVERGENCE: command buffer %d, event %d (A has %d events, B has %d)' % (first['cb'], first['event'], first['a_len'], first['b_len']))
    print('--- A around it:')
    for k, v in first['a']: print('  %-8s %s' % (k, v[:400]))
    print('--- B around it:')
    for k, v in first['b']: print('  %-8s %s' % (k, v[:400]))
    return 1

def selftest():
    def dev_frame(view_at_pass2=1):
        return [
            'CB commit #5 cb 0x7f00 queued',
            'PASS target 3840x2160 surf 12 view 0 load 2 store 1 clear (0 0 0 1) depth 1',
            'PSO 0x1 vs_main_ / ps_main_ vs 0a1b2c3d fs 11223344 | opaque',
            'FTEX[0] 1024x1024 surf 40 view 1', 'FRAGMENT SAMPLER[0] pixel=0 bound=1',
            'DRAW cb 0x7f00 vk 0x9a type 3 | vs vs_main_ | fs ps_main_ | blend off',
            'DISPATCH cb 0x7f00 vk 0x9b cs_main_ | threads 240x135x1 tg 8x8x1',
            'PASS target 1920x1080 surf 13 view 0 load 1 store 1 clear (0 0 0 0) depth 0',
            'FTEX[2] 3840x2160 surf 12 view %d' % view_at_pass2,
            'DRAW cb 0x7f00 vk 0x9c type 3 | vs fsq_vs | fs tonemap_ps | blend off',
            'BLIT tex 1920x1080 surf 13 (0,0 1920x1080) -> tex 1920x1080 surf 2 (0,0)',
            'SURFOUT surf 2 1920x1080 vramOn 1 t=99',
            'CB done #5 status 0',
        ]
    def rel_frame(view_at_pass2=1):
        f = dev_frame(view_at_pass2)
        return [re.sub(r' cb 0x7f00 vk 0x9[abc]', '', l).replace('CB commit #5 cb 0x7f00', 'CB commit #71') for l in f]
    # a request lands mid-frame: the first partial command buffer must be ignored
    head = ['DRAW type 3 | vs late_vs | fs late_ps | blend off', 'CB done #4 status 0']
    a = parse(head + dev_frame() + dev_frame())
    b = parse(rel_frame() + rel_frame())
    n, eq, first = diff(a, b)
    assert n == 2 and eq == 2 and first is None, (n, eq, first)
    print('selftest 1 ok: the two vocabularies of the same frame compare equal (2 command buffers, 0 divergences)')
    b2 = parse(rel_frame() + rel_frame(view_at_pass2=3))
    n, eq, first = diff(a, b2)
    assert first and first['cb'] == 1, first
    k, v = a[1][first['event']]
    assert k == 'PASS' and 'target 1920x1080' in v and 'FTEX 2 3840x2160 1' in v, (k, v)
    assert 'FTEX 2 3840x2160 3' in b2[1][first['event']][1]
    print('selftest 2 ok: the negative control fires at command buffer 1, the 1920x1080 pass whose FTEX[2] view differs')
    assert parse(['DRAW type 3 | vs x | fs y | blend off']) == []
    so = ['SURFOUT surf 45 1920x1080 vramOn 1 t=1', 'PASS target 1280x720 surf 0 view 0 load 1 store 1 clear (0 0 0 1) depth 1',
          'PSO 0x7 vs_main_ / ps_main_ vs ea1d37ea fs 410d3ada | ', 'DRAW type 3 | vs vs_main_ | fs ps_main_ | blend 0 rgb(1,0,op 0) a(1,0,op 0) mask f fmt 58',
          'SURFOUT surf 45 1920x1080 vramOn 1 t=2']
    c = parse(so)
    assert len(c) == 2 and c[0][0][0] == 'PASS' and 'fmt 58' in c[0][0][1] and 'ea1d37ea' in c[0][0][1], c
    print('selftest 4 ok: a trace without CB commit lines is cut at SURFOUT and the DRAW keeps blend state and attachment format')
    print('selftest 3 ok: events before the first command buffer are ignored')
    return 0

if __name__ == '__main__':
    if '--selftest' in sys.argv: sys.exit(selftest())
    args, frames, context, i = [], None, None, 1
    while i < len(sys.argv):
        x = sys.argv[i]
        if x == '--frames': frames = int(sys.argv[i + 1]); i += 2
        elif x == '--context': context = int(sys.argv[i + 1]); i += 2
        elif x.startswith('--'): i += 1
        else: args.append(x); i += 1
    if len(args) != 2: sys.exit(__doc__)
    a = parse(open(args[0], errors='replace')); b = parse(open(args[1], errors='replace'))
    sys.exit(report(a, b, frames, context or 3))
