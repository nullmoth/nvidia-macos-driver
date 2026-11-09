#!/usr/bin/env python3
"""Where libnvencvt (the vendored NVENC / NVDEC codecs) registers, and that the decode service gets its decoders.

Ada had no hardware encoder and no VP9 or AV1 decoder because the library registered only when a user switched it on in
nvrm610.conf; and Safari, QuickTime and FaceTime, which decode inside VTDecoderXPCService, never saw it because that
process registered only the built-in decoders. The selection function is compiled out of the plugin source and run
on a table; the decode service's wiring is checked in the text around it.

usage: test_nvencvt_selection.py [repo root]"""
import pathlib, platform, shutil, subprocess, sys, tempfile

root = pathlib.Path(sys.argv[1] if len(sys.argv) > 1 else pathlib.Path(__file__).resolve().parents[1])
src = (root / 'plugin/NVMTLDevice.m').read_text()
fails = 0


def check(name, ok, detail=''):
    global fails
    print(('ok   ' if ok else 'FAIL ') + name + (f' ({detail})' if detail else ''))
    fails += 0 if ok else 1


def function(signature):
    i = src.find(signature)
    if i < 0:
        return None
    return src[i:src.index('\n}\n', i) + 3]


sel = function('static bool nvmtl_nvencvt_selected(')
gen = function('static bool nvmtl_gen_is(')
check('the plugin has a selection function for libnvencvt', sel is not None)

if sel and shutil.which('clang') and platform.system() == 'Darwin':
    harness = ('#include <stdbool.h>\n#include <stdio.h>\n#include <string.h>\n' + gen + sel +
               'int main(void) {\n'
               '  const char *sels[] = { NULL, "", "0", "1", "obs,Parsec", "VTDecoderXPCService" };\n'
               '  const char *gens[] = { "Ada", "Ampere", "Blackwell", "Turing GTX 16", "Turing", NULL };\n'
               '  const char *procs[] = { "Parsec", "VTDecoderXPCService", "Safari" };\n'
               '  for (int s = 0; s < 6; s++) for (int g = 0; g < 6; g++) for (int p = 0; p < 3; p++)\n'
               '    printf("%d %d %d %d\\n", s, g, p, nvmtl_nvencvt_selected(sels[s], procs[p], gens[g]));\n'
               '  return 0; }\n')
    with tempfile.TemporaryDirectory() as d:
        d = pathlib.Path(d)
        (d / 'h.c').write_text(harness)
        r = subprocess.run(['clang', '-o', str(d / 'h'), str(d / 'h.c')], capture_output=True, text=True)
        check('the selection function compiles on its own', r.returncode == 0, r.stderr.strip()[-300:])
        if r.returncode == 0:
            out = subprocess.run([str(d / 'h')], capture_output=True, text=True).stdout.split('\n')
            got = {tuple(map(int, l.split()[:3])): bool(int(l.split()[3])) for l in out if l.strip()}
            # sels: 0 unset, 1 empty, 2 "0", 3 "1", 4 list, 5 decode service named
            # gens: 0 Ada, 1 Ampere, 2 Blackwell, 3 Turing GTX 16, 4 Turing, 5 none published
            # procs: 0 Parsec, 1 VTDecoderXPCService, 2 Safari
            check('unset: Ada and Ampere get the codecs in every process', all(got[(0, g, p)] for g in (0, 1) for p in range(3)))
            check('unset: Blackwell, Turing and an unknown card get nothing', not any(got[(0, g, p)] for g in (2, 3, 4, 5) for p in range(3)))
            check('an empty value is the same as unset', all(got[(1, g, p)] == got[(0, g, p)] for g in range(6) for p in range(3)))
            check('0 turns it off everywhere, Ada included', not any(got[(2, g, p)] for g in range(6) for p in range(3)))
            check('1 turns it on for every card and process, GTX 16 included', all(got[(3, g, p)] for g in range(6) for p in range(3)))
            check('a list registers only in the processes it names', got[(4, 0, 0)] and not got[(4, 0, 1)] and not got[(4, 0, 2)])
            check('naming the decode service registers there and nowhere else', got[(5, 5, 1)] and not got[(5, 0, 0)])

reg = function('static void nvmtl_nvencvt_register(')
svc = function('static void nvmtl_nvdec_in_decode_service(')
check('the registrar can register decoders alone', reg is not None and 'NVEncVT_RegisterDecoders' in reg)
check('the decode service asks for libnvencvt after the built-in decoders',
      svc is not None and 'nvmtl_nvencvt_register(true)' in svc
      and svc.index('nvmtl_nvdec_hevc_register()') < svc.index('nvmtl_nvencvt_register(true)'))
check('the decode service reads nvrm610.conf before it reads any selector',
      svc is not None and 'nvmtl_conf_settle()' in svc and svc.index('nvmtl_conf_settle()') < svc.index('getenv("NVMTL_NO_NVDEC")'))
check('the decode service registers on a queue that is not starved under load',
      svc is not None and 'QOS_CLASS_USER_INITIATED' in svc and 'QOS_CLASS_UTILITY' not in svc)
check('the built-in encoders declare low-latency rate control only when NVMTL_ENCODER_LLRC asks',
      'NVMTL_ENCODER_LLRC' in src and src.count('nvmtl_encoder_registration_info(@"com.nvmtl.videoencoder') == 2
      and '"EnableLowLatencyRateControl"' in src or '@"EnableLowLatencyRateControl"' in src)
check('device initialisation still registers the full library', 'nvmtl_nvencvt_register(false)' in src)

print(f'{fails} failed' if fails else 'all passed')
sys.exit(1 if fails else 0)
