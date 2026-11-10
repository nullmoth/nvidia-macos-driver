#!/usr/bin/env python3
"""A user client's token-mapping table costs what the client holds, not its limit.

The first token mapping on a client allocated the table at its limit, 4096 entries. Any local user may open thousands
of clients (28672 are open to accounts that are not system accounts), and one mapping on each pinned a full table for
each: about 1.8 GB of kernel memory for no more than a page of mapped memory per client. The table now starts at 8
entries and doubles. The sizing rule is compiled from kexts/NVRM/nvrm_tok_table.h and run; the kext's use of it is
checked in the source, with the revision before as the control.

usage: test_nvrm_token_table.py [repo root]"""
import pathlib, platform, re, shutil, subprocess, sys, tempfile

root = pathlib.Path(sys.argv[1] if len(sys.argv) > 1 else pathlib.Path(__file__).resolve().parents[1])
BEFORE = '4f4f70d6'
fails = 0


def check(name, ok, detail=''):
    global fails
    print(('ok   ' if ok else 'FAIL ') + name + (f' ({detail})' if detail else ''))
    fails += 0 if ok else 1


hdr = root / 'kexts/NVRM/nvrm_tok_table.h'
src = (root / 'kexts/NVRM/NVRM.cpp').read_text()
FIXED = 'kern_os_malloc(sizeof(NvMapToken) * kNvMapTokensPerClient)'
check('the sizing rule has its own header', hdr.exists())
check('the kext sizes the table by that rule', 'nvrm_tok_next_capacity(fTokCap, fNTok, kNvMapTokensPerClient)' in src)
check('the kext never allocates the table at its limit', FIXED not in src)
check('entries already held move into the larger table', 'nvu_memcpy(grown, fTok, sizeof(NvMapToken) * fNTok)' in src)
check('the larger table is installed under the lock the lookups take',
      src.index('IOLockLock(fMapLock);\n        if (fMapList != snapNode') < src.index('fTok = grown; fTokCap = cap;') < src.index('fTok[fNTok].token = type;'))
check('a freed table forgets its capacity', 'fTok = nullptr; fNTok = 0; fTokCap = 0;' in src)
old = subprocess.run(['git', '-C', str(root), 'show', f'{BEFORE}:kexts/NVRM/NVRM.cpp'], capture_output=True, text=True)
if old.returncode:
    print('skip the control: this is not a full clone')
else:
    check('control: the revision before allocated every table at its limit', FIXED in old.stdout)

m = re.search(r'kNvMapTokensPerClient = (\d+)', src)
limit = int(m.group(1)) if m else 0
check('the limit per client is unchanged', limit == 4096)

if hdr.exists() and shutil.which('clang') and platform.system() == 'Darwin':
    prog = ('#include <stdio.h>\n#include "%s"\n' % hdr +
            'int main(void) {\n'
            '  unsigned cap = 0, held = 0, grows = 0, bad = 0, first = 0;\n'
            '  for (;;) { unsigned c = nvrm_tok_next_capacity(cap, held, %d); if (!c) break;\n' % limit +
            '    if (c < cap || c <= held || c > %d) bad++;\n' % limit +
            '    if (c != cap) { grows++; cap = c; if (!first) first = c; }\n'
            '    held++; }\n'
            '  printf("%u %u %u %u %u ", first, held, cap, grows, bad);\n'
            '  printf("%u %u %u\\n", nvrm_tok_next_capacity(16, 3, 4096), nvrm_tok_next_capacity(4096, 4096, 4096), nvrm_tok_next_capacity(0x80000000u, 0x80000000u, 0xffffffffu));\n'
            '  return 0; }\n')
    with tempfile.TemporaryDirectory() as d:
        d = pathlib.Path(d); (d / 't.c').write_text(prog)
        r = subprocess.run(['clang', '-Wall', '-Werror', '-o', str(d / 't'), str(d / 't.c')], capture_output=True, text=True)
        check('the rule compiles without warnings', r.returncode == 0, r.stderr[-300:])
        if r.returncode == 0:
            got = subprocess.run([str(d / 't')], capture_output=True, text=True).stdout.split()
            check('one mapping takes a table of 8 entries, not 4096', got[0] == '8', got[0])
            check('the table reaches the limit and then refuses', got[1] == str(limit) and got[2] == str(limit), ' '.join(got[1:3]))
            check('it gets there in ten allocations, never shrinking and never past the limit', got[3] == '10' and got[4] == '0', ' '.join(got[3:5]))
            check('a table with room keeps its size; a full one at the limit refuses; doubling cannot wrap',
                  got[5:] == ['16', '0', str(0xffffffff)], ' '.join(got[5:]))
    # what the clients open to ordinary accounts can pin with one mapping each (16-byte entries)
    clients = 32768 - 4096
    check('one mapping on every client an ordinary account can open pins megabytes, not gigabytes',
          clients * 8 * 16 < 8 << 20 < clients * limit * 16, f'{clients * 8 * 16 >> 20} MB now, {clients * limit * 16 >> 20} MB before')
else:
    print('SKIP the sizing arms need clang on a Mac')

print(f'{fails} failed' if fails else 'all passed')
sys.exit(1 if fails else 0)
