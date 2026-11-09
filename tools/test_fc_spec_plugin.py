#!/usr/bin/env python3
"""The one-module function-constant path is opt-in and can never take a variant away from the per-variant path.
Source-level arms (the plugin cannot be linked on a host): off unless NVMTL_FC_SPEC=1; the path runs only when both
translator entry points are present; any failure falls back to the per-variant translation; the cache entry proves its
own layout; the plugin's own SpecIds stay clear of the function-constant range."""
import pathlib, re, sys
root = pathlib.Path(sys.argv[1] if len(sys.argv) > 1 else pathlib.Path(__file__).resolve().parents[1])
src = (root / 'plugin/NVMTLLibraryLoad.m').read_text()
tr = (root / 'translator/translator/src/fc_specialize.rs').read_text()
fails = 0
def check(name, ok):
    global fails
    print(('ok   ' if ok else 'FAIL ') + name); fails += not ok
def body(head):
    i = src.find(head)
    return src[i:src.index('\n}\n', i)] if i >= 0 else ''
on = body('static BOOL nvmtl_fc_spec_on(void)')
check('the path is off unless NVMTL_FC_SPEC=1', 'e && e[0] == \'1\'' in on and 'NVMTL_FC_SPEC' in on)
w = body('NSData *nvmtl_translate_with_fc(NSString *ll')
check('the path needs both translator entry points', 'xlate_spec && xlate_spec_apply' in w)
check('the path is tried only for a set that has constants', 'n > 0 && nvmtl_fc_spec_on()' in w)
check('a refusal falls through to the per-variant translation (the xlate_fc call is still there, after it)',
      'declined' in w and w.index('declined') < w.index('xlate_fc('))
mod = body('static BOOL nvmtl_fc_spec_module(')
check('a cache entry is used only with its magic and a table of whole entries', '0x31435053u' in mod and '% 20 != 0' in mod)
check('the entry is written through the footer-checked cache writer', 'nvmtl_cache_write(' in mod)
check('the translator keeps the function-constant SpecIds above the plugin\'s own (SpecId 3 is the linearization)',
      'FC_SPEC_ID_BASE: u32 = 0x4000' in tr)
check('dlsym loads both entry points', 'dlsym(h, "nvmtl_translate_spec")' in src and 'dlsym(h, "nvmtl_spec_apply")' in src)
sys.exit(1 if fails else 0)
