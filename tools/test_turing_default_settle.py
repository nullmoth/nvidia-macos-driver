#!/usr/bin/env python3
"""Default GSP settle chosen by NVRM itself, compiled from the kext source and run on device ids from user logs.

Turing needs about ten seconds before the display arms. Only the installer asked for it, and it matched display-class
cards only, so a muxless laptop card or a machine with rewritten boot arguments started with the 0.5 s default. The
default now follows the device id; a card whose BAR1 could not be placed keeps its 100 s wait. The negative control
runs the previous expression and shows a Turing card regress to 0.5 s."""
import pathlib
import subprocess
import sys
import tempfile

root = pathlib.Path(sys.argv[1] if len(sys.argv) > 1 else pathlib.Path(__file__).resolve().parents[1])
src = (root / 'kexts/NVRM/NVRM.cpp').read_text()
h0 = src.index('static inline bool nvrmIsTuringDeviceId')
helper = src[h0:src.index('\n}\n', h0) + 3]
line = [l for l in src.splitlines() if l.strip().startswith('fAutoGoSettleMs = bar1Placed')]
assert len(line) == 1, 'expected exactly one default-settle line'
fails = 0


def check(name, ok, detail=''):
    global fails
    print(('ok   ' if ok else 'FAIL ') + name + (f' ({detail})' if detail else ''))
    fails += not ok


def run(expr, cases):
    prog = ('#include <stdio.h>\n' + helper +
            'int main(){unsigned ids[]={%s};int placed[]={%s};for(int i=0;i<%d;i++){unsigned fAutoGoSettleMs;'
            'int bar1Placed=placed[i];struct{unsigned device_id;} pci={ids[i]};struct{decltype(pci)&pci_info;} n{pci};'
            'auto *nv=&n;%s printf("%%u\\n",fAutoGoSettleMs);}}\n') % (
        ','.join(hex(c[0]) for c in cases), ','.join(str(c[1]) for c in cases), len(cases), expr)
    with tempfile.TemporaryDirectory() as d:
        f = pathlib.Path(d, 't.cpp'); f.write_text(prog)
        subprocess.run(['/usr/bin/clang++', '-std=c++17', '-o', d + '/t', str(f)], check=True)
        return [int(x) for x in subprocess.run([d + '/t'], capture_output=True, text=True, check=True).stdout.split()]


cases = [(0x1F08, 1), (0x1F15, 1), (0x1E87, 1), (0x2191, 1), (0x2504, 1), (0x2D04, 1), (0x1F08, 0), (0x2504, 0),
         (0x1F82, 1), (0x1DFF, 1), (0x2000, 1), (0x217F, 1), (0x2200, 1), (0x2882, 1)]
new = run(line[0].strip(), cases)
check('RTX 2060 desktop (1F08), BAR1 placed: 10 s', new[0] == 10000, new[0])
check('RTX 2060 Mobile (1F15): 10 s', new[1] == 10000, new[1])
check('RTX 2080 (1E87): 10 s', new[2] == 10000, new[2])
check('GTX 1660 Ti (TU116, 2191): 10 s', new[3] == 10000, new[3])
check('Ampere (2504) keeps 0.5 s', new[4] == 500, new[4])
check('Blackwell (2D04) keeps 0.5 s', new[5] == 500, new[5])
check('Turing with BAR1 not placed keeps the 100 s fallback', new[6] == 100000, new[6])
check('non-Turing with BAR1 not placed keeps 100 s', new[7] == 100000, new[7])
for nm, i, want in (('TU117 (1F82)', 8, 10000), ('just below 1E00 (1DFF)', 9, 500), ('above 1FFF (2000, GA/Volta gap)', 10, 500),
                    ('below TU116 range (217F)', 11, 500), ('above TU116 range (2200)', 12, 500), ('Ada (2882)', 13, 500)):
    check(nm + ' -> %d ms' % want, new[i] == want, new[i])
old = run('fAutoGoSettleMs = bar1Placed ? 500 : 100000;', cases)
check('negative control: the previous default gives a Turing card 0.5 s', old[0] == 500, old[0])
sys.exit(1 if fails else 0)
