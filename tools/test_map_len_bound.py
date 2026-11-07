#!/usr/bin/env python3
"""A user mapping of system memory is bounded by the length RM validated for it, never by the end of the allocation.

NVIDIA 620 stopped carrying a size in the mapping context (nv_usermap_access_params_t and access_size are gone) and
osMapSystemMemory works on the ROOT allocation, which can hold other objects past the one being mapped. The size now
reaches us one call later: on the same thread, RM calls nv_alloc_user_mapping with the Length it validated. These checks
pin each link of that chain in the source, and every check is also run on a copy with that link broken, which it must
reject (a guard that cannot fail proves nothing)."""
import pathlib
import re
import sys

root = pathlib.Path(sys.argv[1] if len(sys.argv) > 1 else pathlib.Path(__file__).resolve().parents[1])
nvrm = (root / 'kexts/NVRM/NVRM.cpp').read_text()
osx = (root / 'kexts/NVRM/os-xnu2.cpp').read_text()
fails = 0


def check(name, ok):
    global fails
    print(('ok   ' if ok else 'FAIL ') + name)
    fails += 0 if ok else 1


def sysmem_branch(src):
    m = re.search(r'if \(fIsCtl\) \{\n(.*?)\n    \} else \{', src, re.S)
    return m.group(1) if m else ''


def refuses_without_length(src):
    b = sysmem_branch(src)
    return bool(re.search(r'if \(snapLen == 0\) \{[^}]*return kIOReturnNotPermitted;', b))


def clamps_to_length(src):
    b = sysmem_branch(src)
    takes = re.search(r'NvU64 len = \(snapLen \+ PAGE_SIZE - 1\) & ~\(NvU64\)\(PAGE_SIZE - 1\);', b)
    caps = re.search(r'if \(len > bytes - off\) len = bytes - off;', b)
    widens = re.search(r'NvU64 len = bytes - off;', b)
    return bool(takes and caps and not widens)


def length_read_under_lock(src):
    # From the function's own body: the copy of the context and of the length must sit between the lock and the
    # unlock that follows the copy (the earlier unlock belongs to the no-context early return).
    body = src[src.find('IOReturn NVRMUserClient::clientMemoryForType('):]
    m = re.search(r'IOLockLock\(fMapLock\);.*?snapshot = fMapList->context;(.*?)IOLockUnlock\(fMapLock\);', body, re.S)
    return bool(m and 'snapLen = (fMapLenNode == fMapList) ? fMapLen : 0;' in m.group(1))


def length_reported(src):
    m = re.search(r'NV_STATUS NV_API_CALL nv_alloc_user_mapping\(.*?\n\}\n', src, re.S)
    return bool(m and 'nv_xnu_note_map_len(validated)' in m.group(0) and '__builtin_add_overflow' in m.group(0)
                and 'nv_xnu_note_map_len(0)' in m.group(0))


def arms_only_new_node(src):
    m = re.search(r'void NV_API_CALL nv_release_file_va\(.*?\n\}\n', src, re.S)
    return bool(m and 'p->wasEmpty && now' in m.group(0) and 'if (now != c->mapLenNode())' in m.group(0))


def length_reset_with_list(src):
    return src.count('nvMapListFree(&fMapList);\n    fMapLen = 0; fMapLenNode = nullptr; fMapGen++;') == 1 and \
           src.count('nvMapListFree(&me->fMapList);\n            me->fMapLen = 0; me->fMapLenNode = nullptr; me->fMapGen++;') == 1


def applied_to_stored_node_only(src):
    m = re.search(r'extern "C" void nv_xnu_note_map_len\(NvU64 bytes\)\n\{.*?\n\}\n', src, re.S)
    return bool(m and 'if (*c->mapList() == node && *c->mapGen() == gen && c->mapLenNode() == nullptr)' in m.group(0)
                and 'c->release();' in m.group(0))


def generation_bumps_on_every_head_change(src):
    return (src.count('nvMapListFree(&fMapList);\n    fMapLen = 0; fMapLenNode = nullptr; fMapGen++;') == 1 and
            src.count('nvMapListFree(&me->fMapList);\n            me->fMapLen = 0; me->fMapLenNode = nullptr; me->fMapGen++;') == 1 and
            'if (now != *c->mapHeadAtAcquire()) ++*c->mapGen();' in src and
            '*c->mapHeadAtAcquire() = *c->mapList();' in src)


def slot_scoped_to_escape(src):
    m = re.search(r'IOReturn NVRMUserClient::esc\(uint32_t cmd, void \*data, uint32_t size\)\n\{\n    Gate g\(fGate\);\n(.*?)\n', src)
    scope = re.search(r'struct NvMapPendingScope \{ NvMapPendingScope\(\) \{ mapPendingClearThread\(\); \} '
                      r'~NvMapPendingScope\(\) \{ mapPendingClearThread\(\); \} \};', src)
    return bool(m and 'NvMapPendingScope pendingScope;' in m.group(1) and scope)


arms = [
    ('a system-memory map with no validated length is refused', refuses_without_length, nvrm,
     lambda s: s.replace('if (snapLen == 0) {', 'if (0) {')),
    ('the mapped length is the validated length, capped by the allocation, never the allocation end', clamps_to_length,
     nvrm, lambda s: s.replace('NvU64 len = (snapLen + PAGE_SIZE - 1) & ~(NvU64)(PAGE_SIZE - 1);', 'NvU64 len = bytes - off;')),
    ('the length is read in the same locked snapshot as the context', length_read_under_lock, nvrm,
     lambda s: s.replace('    snapLen = (fMapLenNode == fMapList) ? fMapLen : 0;', '    snapLen = fMapLen;')),
    ('nv_alloc_user_mapping reports pageOffset + Length, and 0 on overflow', length_reported, osx,
     lambda s: s.replace('nv_xnu_note_map_len(validated)', '(void)validated')),
    ('only a node stored during this hold is armed, and a changed node drops the old length', arms_only_new_node, nvrm,
     lambda s: s.replace('p->wasEmpty && now', 'now')),
    ('a reported length lands only on the node this thread stored, and only while it is current',
     applied_to_stored_node_only, nvrm,
     lambda s: s.replace('if (*c->mapList() == node && *c->mapGen() == gen && c->mapLenNode() == nullptr)',
                         'if (*c->mapList() == node && c->mapLenNode() == nullptr)')),
    ('a freed node address coming back as a new node cannot inherit a length (generation bumps on store and free)',
     generation_bumps_on_every_head_change, nvrm,
     lambda s: s.replace('if (now != *c->mapHeadAtAcquire()) ++*c->mapGen();', '')),
    ('a freed list also moves the generation', generation_bumps_on_every_head_change, nvrm,
     lambda s: s.replace('fMapLen = 0; fMapLenNode = nullptr; fMapGen++;', 'fMapLen = 0; fMapLenNode = nullptr;')),
    ('a pending slot lives for one escape: cleared on entry and on every exit', slot_scoped_to_escape, nvrm,
     lambda s: s.replace('    NvMapPendingScope pendingScope;', '    (void)0;')),
    ('a freed or re-bound mapping list forgets its length', length_reset_with_list, nvrm,
     lambda s: s.replace('nvMapListFree(&fMapList);\n    fMapLen = 0; fMapLenNode = nullptr; fMapGen++;', 'nvMapListFree(&fMapList);')),
]
for name, fn, src, breaker in arms:
    check(name, fn(src))
    broken = breaker(src)
    check('  negative control: the check rejects a source with this link broken', broken != src and not fn(broken))

print(f'{fails} failed')
sys.exit(1 if fails else 0)
