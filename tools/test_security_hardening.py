#!/usr/bin/env python3
"""The driver's part of the security batch from the external review of 1.15.0, checked without the card.

- The scripts that run as root (install.sh, uninstall.sh) fix PATH and stay executable.
- The kernel gates are present in the source (the binary and the live boot are checked on the card).

The 1401 app's part (login items, log collection, hardened runtime) is tested in 1401 (mac/tools)."""
import os
import pathlib
import sys

root = pathlib.Path(sys.argv[1] if len(sys.argv) > 1 else pathlib.Path(__file__).resolve().parents[1])
fails = 0


def check(name, ok, detail=''):
    global fails
    print(('ok   ' if ok else 'FAIL ') + name + (f' ({detail})' if detail else ''))
    fails += not ok


for f in ('package/install.sh', 'package/uninstall.sh'):
    text = (root / f).read_text()
    check(f'{f} fixes PATH before running anything',
          'PATH=/usr/bin:/bin:/usr/sbin:/sbin; export PATH' in '\n'.join(text.splitlines()[:12]))
    check(f'{f} is still executable', os.access(root / f, os.X_OK))

# --- kernel gates in the source ---
nvrm = (root / 'kexts/NVRM/NVRM.cpp').read_text()
osx = (root / 'kexts/NVRM/os-xnu.cpp').read_text()
check('RM administrator check reads the caller uid', 'return (kauth_getuid() == 0) ? NV_TRUE : NV_FALSE;' in osx
      and 'os_is_administrator(void) { NV_XNU_TRC; return NV_TRUE; }' not in osx)
check('first-time GPU bring-up refuses non-root callers', 'if (pass > fPassDone && kauth_getuid() != 0)' in nvrm)
check('event waiters are capped', 'kNvEvtMaxSleepers = 256' in nvrm and 'OSDecrementAtomic(&gNvEvtSleepers)' in nvrm)
# NVIDIA 620 replaced the one per-client context (fMmap) with RM's list of mapping nodes (fMapList). A re-bind drops
# every pending node, valid or not, so nothing an earlier fd parked can be mapped through the new one.
def bind_clears(src):
    return ('if (v > 0x7FFFFFFFLL || v < -0x80000000LL) return kIOReturnBadArgument;' in src
            and 'if (me->fBound >= 0) {\n        IOLockLock(me->fMapLock);\n        if (me->fMapList) {' in src
            and 'nvMapListFree(&me->fMapList);' in src)
check('bind-fd rejects out-of-range values and clears a stale context when bound to a real fd', bind_clears(nvrm))
check('negative control: bind-fd without the range check is caught',
      not bind_clears(nvrm.replace('if (v > 0x7FFFFFFFLL || v < -0x80000000LL) return kIOReturnBadArgument;', '')))
check('negative control: a bind that keeps the stale list is caught',
      not bind_clears(nvrm.replace('nvMapListFree(&me->fMapList);', '')))

# --- 2.3.7: the gates added after the driver was audited against Apple's and NVIDIA's own bug classes ---
fb = (root / 'kexts/NVRMFB/fb/nvrm-fb.cpp').read_text()
vk = (root / 'plugin/nvmtl_vk.c').read_text()
accel = (root / 'kexts/NVRM/accel/nvrm-accel.cpp').read_text()
osx2 = (root / 'kexts/NVRM/os-xnu2.cpp').read_text()
gva = (root / 'kexts/NVRM/nvrm_gpuva.cpp').read_text()

# Any process can read an IOKit registry property, so the trace must not be published by default: it
# carried kext text and kernel virtual addresses, measured on a live machine as an ordinary user.
check('the framebuffer trace is published only with boot-arg nvfbtrace',
      'PE_parse_boot_argn("nvfbtrace"' in fb and '!me || !nvfbTracePublish()' in fb)
# Written so it runs on any tree, fixed or not: a marker-split would raise on an unfixed one, and a
# check that cannot run must fail, never pass.
ptr_lines = [l.strip() for l in fb.splitlines()
             if '%p' in l and ('FBLOG(' in l or 'IOLog(' in l or 'kprintf(' in l)]
check('no framebuffer log line prints a pointer', not ptr_lines,
      f'{len(ptr_lines)} line(s) still do, first: ' + (ptr_lines[0][:60] if ptr_lines else ''))

# x + w wrapped in 32 bits, so a region starting near 2^32 passed the bounds test and reached the GPU
# copy as a negative image offset. The read path handed memory from outside the texture back to the app.
for fn in ('image_write_region', 'image_read_region'):
    seg = vk.split(f'int nvmtl_vk_{fn}_level_layer')[1][:1200]
    check(f'{fn} bounds the region by subtraction, not x + w',
          'w > lw - x' in seg and 'h > lh - y' in seg and 'x + w > lw' not in seg)

# plane is a full uint32 from the caller and reached deviceCacheForIOSurface before any clamp.
# Anchor on the mangled call, not the readable name: the comment above the clamp names the function too,
# and an earlier version of this arm matched that comment and passed while measuring nothing.
sv = accel.split('static IOReturn nvAccelSurfVram')[1][:2600]
call = '_ZN22IOGraphicsAccelerator223deviceCacheForIOSurface'
check('the surface escape bounds the caller plane before the cache lookup',
      'if (p->plane > 15)' in sv and call in sv
      and sv.index('if (p->plane > 15)') < sv.index(call))

# off + len wraps for a near-2^64 access size, and IOSubMemoryDescriptor's own test wraps identically.
check('the sysmem mapping clamp subtracts instead of adding',
      'if (len > bytes - off)' in nvrm and 'if (off + len > bytes)' not in nvrm)

# The firmware file is outside the kernel's control: 0 bytes gave RM an empty image and >=2 GiB made the
# length handed to vn_rdwr negative.
check('the firmware read refuses an empty or oversized image',
      'NV_XNU_FW_MAX_BYTES' in osx2 and 'va.va_data_size == 0' in osx2)

# Identity comes from the kernel's own record, never from the request.
check('GPU address resolution never takes the caller handles',
      'if (r->hClient && r->hDevice) return true;' not in gva)

# A client's own teardown ran with nothing holding it off an in-flight method: clientClose freed the
# array of physical ranges while clientMemoryForType was handing its entries to withPhysicalAddress.
check('the cleanup flag cannot be won by two threads', 'OSCompareAndSwap(0, 1, &fCleaned)' in nvrm)
check('each client has a gate held across its methods and its teardown',
      'IOLock *fGate' in nvrm and nvrm.count('Gate g(fGate);') >= 3)
# 620: the ranges hang off RM's mapping nodes and go back through os_free_mem in nvMapListFree.
def frees_in_free_only(src):
    if 'void NVRMUserClient::free(void)' not in src or 'IOReturn NVRMUserClient::clientClose' not in src:
        return False
    fr = src.split('void NVRMUserClient::free(void)')[1].split('\n}\n')[0]
    cl = src.split('IOReturn NVRMUserClient::clientClose')[1].split('terminate();')[0]
    return ('nvMapListFree(&fMapList);' in fr
            and not any(k in cl for k in ('nvMapListFree', 'os_free_mem', 'kern_os_free')))
check('the mapping ranges are released in free(), not in clientClose', frees_in_free_only(nvrm))
check('negative control: a close that frees the list is caught',
      not frees_in_free_only(nvrm.replace('        if (fMapList) fMapList->context.valid = NV_FALSE;\n',
                                          '        if (fMapList) fMapList->context.valid = NV_FALSE;\n        nvMapListFree(&fMapList);\n', 1)))

# A card range mapped into a process must at least land inside one of this card's bars, so a caller
# cannot hand over an arbitrary physical address and have it mapped.
# This arm used to require the framebuffer bar ALONE. That shipped to the test Mac and WindowServer
# crash-looped: CoreDisplay maps phys 0x80bb0000 out of bar0 and aborted in
# CoreDisplay_CreateDisplayForCGXDisplayDevice when it was refused. The bar bound is the real check;
# asserting anything tighter here asserts a driver that cannot drive a display.
_dev_branch = nvrm.split('NvU64 n = c->memArea.numRanges;')[1].split('IOAddressRange *r')[0]
check('a device client maps only inside this card\'s bars',
      'nv_pm_is_gpu(' in _dev_branch and 'nv_pm_dev_refused' in _dev_branch)
check('the framebuffer-only refusal is gone (it broke CoreDisplay on real hardware)',
      'nv_pm_is_gpu_fb' not in _dev_branch
      and 'outside the framebuffer aperture' not in nvrm)

# The kernel read these images from /Users/Shared, which every user can write, and checked nothing.
check('the firmware is read from a root-only directory',
      '/Library/NullMoth/nvfw' in osx2 and '/Users/Shared/nvfw' not in osx2)
check('the firmware is checked against what the package shipped',
      'nv_xnu_fw_set[]' in osx2 and 'nv_xnu_fw_digest_ok' in osx2 and 'gsp_ga10x.bin' in osx2)
check('the firmware size cap admits the largest image we ship (84 MB Ampere GSP)',
      'NV_XNU_FW_MAX_BYTES (256u' in osx2)

inst = (root / 'package/install.sh').read_text()
uninst = (root / 'package/uninstall.sh').read_text()
check('the installer owns the firmware directory and takes write away',
      'chown -R root:wheel "$FW"' in inst and 'chmod -R go-w,a+rX "$FW"' in inst)
check('the installer refuses a firmware directory it did not make',
      '! -user root' in inst and 'FWOLD' in inst)
check('uninstall removes the firmware from both locations',
      '/Library/NullMoth/nvfw' in uninst and '/Users/Shared/nvfw' in uninst)

lib = (root / 'plugin/NVMTLLibraryLoad.m').read_text()
obj = (root / 'plugin/NVMTLObjects.m').read_text()

# A shader library is attacker bytes and LLVM's bitcode reader is not written to withstand them.
check('shader bitcode is capped before the in-process reader sees it',
      'NVMTL_AIR_MAX_BYTES' in lib and 'air.length > NVMTL_AIR_MAX_BYTES' in lib)
# The handler used to only log, so a module the reader rejected was compiled anyway. It must key off
# severity: the same handler carries warnings, and failing on those would refuse working shaders.
check('a reader error refuses the module, and only an error does',
      'gAirError' in lib and 'GetDiagInfoSeverity' in lib
      and '&& !gAirError)' in lib)
check('the severity entry point is optional, so its absence does not disable the reader',
      'dlsym(library, "LLVMGetDiagInfoSeverity")' in lib
      and 'NV_LLVM_LOAD(LLVMGetDiagInfoSeverity)' not in lib)
# The binding path has always checked this; the encoder path builds an address for a shader to dereference.
check('the argument encoder bounds the offset against the buffer length',
      'off > nb->_b.size' in obj)

# An opt-in external encoder is dlopened into every app that encodes, so the library and the directory
# holding it must be root-owned and not group/other-writable. Measured on the test Mac in both
# directions: absent REFUSE, user-owned REFUSE, user-owned inside the root dir REFUSE, root-owned
# go-w-clear ALLOW, root-owned group-writable REFUSE.
dev = (root / 'plugin/NVMTLDevice.m').read_text()
_nvcvt = dev.split('static void nvmtl_nvencvt_register')[1].split('\n}\n')[0] if 'nvmtl_nvencvt_register' in dev else ''
_guard = dev.split('static bool nvmtl_loadable_root_only')[1].split('\n}\n')[0] if 'nvmtl_loadable_root_only' in dev else ''
check('the external encoder loads only from a root-owned, non-writable path',
      bool(_guard) and _guard.count('lstat(') == 2          # the file AND its directory
      and 'st.st_uid != 0' in _guard and 'ds.st_uid == 0' in _guard
      and _guard.count('S_IWGRP | S_IWOTH') == 2
      and 'S_ISREG' in _guard and 'S_ISDIR' in _guard)
check('the external encoder follows NVMTL_NVENCVT: automatic on Ada and Ampere, 0 turns it off, a list names processes',
      bool(_nvcvt) and 'getenv("NVMTL_NVENCVT")' in _nvcvt and 'return;' in _nvcvt
      and 'strcmp(sel, "0")) return false' in dev)
check('the external encoder stays out of WindowServer, like the other codec registrars',
      bool(_nvcvt) and 'strcmp(me, "WindowServer")' in _nvcvt)

# Metal's region fields are 64-bit and the copy path takes 32-bit, so a value that does not fit used to be
# narrowed into a different, valid-looking region: an origin of 2^32 read as 0 and the copy landed at the
# start of the texture instead of being refused.
_fits = obj.split('static BOOL nvmtl_region_fits')[1].split('\n}\n')[0] if 'nvmtl_region_fits' in obj else ''
check('a region that does not fit the copy path is refused before anything is narrowed',
      bool(_fits) and _fits.count('UINT32_MAX') == 8          # 3 origin + 3 size + level + slice
      and obj.count('if (!nvmtl_region_fits(') == 4)          # both getBytes and both replaceRegion
check('the texture-buffer region is bounded by subtraction, not by adding origin and width',
      obj.count('region.size.width > _i.w - region.origin.x') == 2
      and 'region.origin.x + region.size.width > _i.w' not in obj)

# Two mappings could take the same slot, and a release could clear an entry a lookup was reading.
_mapfn = osx.split('void *NV_API_CALL os_map_kernel_space')[1].split('NvU64 nv_xnu_phys_for_kernel_va')[0]
_unmapfn = osx.split('void NV_API_CALL os_unmap_kernel_space')[1].split('int nv_xnu_kmap_overlap')[0]
check('the kernel mapping table serialises the paths that allocate and clear a slot',
      'nv_xnu_maps_lock = IOLockAlloc()' in osx
      and _mapfn.count('IOLockLock(nv_xnu_maps_lock)') == 2      # the alias path and the mapped path
      and _mapfn.count('IOLockUnlock(nv_xnu_maps_lock)') == 3    # one per return path out of each loop
      and 'IOLockLock(nv_xnu_maps_lock)' in _unmapfn)
check('the driver refuses to start if that lock cannot be allocated',
      'if (!nv_xnu_maps_init())' in nvrm)
check('a slot publishes its address last and clears it first, so a lookup never sees a partial entry',
      '__atomic_store_n(&m->va, va, __ATOMIC_RELEASE)' in osx
      and '__atomic_store_n(&nv_xnu_maps[i].va, (void *)NULL, __ATOMIC_RELEASE)' in _unmapfn)
# The other direction, and the reason the readers are ordered rather than locked: they are also reached from
# the framebuffer's interrupt path, where taking a mutex is not allowed.
_lookup = osx.split('NvU64 nv_xnu_phys_for_kernel_va')[1].split('void NV_API_CALL os_unmap_kernel_space')[0]
_overlap = osx.split('int nv_xnu_kmap_overlap')[1].split('\n}\n')[0]
check('the two lookups take no mutex (they run in the interrupt path) and read the address with acquire',
      'IOLock' not in _lookup and 'IOLock' not in _overlap
      and _lookup.count('__ATOMIC_ACQUIRE') == 1 and _overlap.count('__ATOMIC_ACQUIRE') == 1)

# Whether a function needs its linked visible functions was decided by reading the translator's error text.
# The translator's wording changed, nothing matched, the function came back nil, and the system's icon and
# glass shaders drew nothing. The decision now asks the translator for the references, with the old wordings
# only as a fallback for a translator that cannot answer.
_vl = lib.split('BOOL nvmtl_needs_visible_link(NSString *air, NSString *why)\n{')[1].split('\n}\n')[0] \
      if 'BOOL nvmtl_needs_visible_link(NSString *air, NSString *why)\n{' in lib else ''
check('deferring a function to pipeline creation asks the translator for its visible references first',
      bool(_vl) and _vl.index('xlate_vrefs(') < _vl.index('rangeOfString:')
      and 'no linked function supplies it' in _vl)
check('no caller decides it from the error text any more',
      # two callers pass the error text as the fallback; the link-first probe passes nil (no text at all)
      obj.count('nvmtl_needs_visible_link(_airs[name]') == 3 and obj.count('nvmtl_needs_visible_link(_airs[name], nil)') == 1
      and 'rangeOfString:@"visible function reference"' not in obj
      and 'rangeOfString:@"Metal visible function table"' not in obj)
check('the expected first attempt is not logged as a failure, and only on the path that defers',
      'static __thread int g_fc_expect_link;' in lib
      and obj.count('nvmtl_fc_expect_link(1);') == 1 and obj.count('nvmtl_fc_expect_link(0);') == 1)

# The installer checks and installs a root-owned private copy of the package, so a kext swapped in the user's folder
# after the check cannot reach /Library/Extensions. The accelerator's path is computed before that copy exists, so it
# has to be recomputed after the redirect or that one kernel extension still comes from the unchecked folder.
_redirect = inst.find('HERE="$STAGE/pkgroot"')
check('every kext, the accelerator included, is installed from the checked private copy',
      _redirect > 0 and inst.find('VAR="$HERE/Library/NullMoth/kexts/$MAJ"', _redirect) > _redirect)

# Moving the firmware broke the way out. Both restore paths are gated on $BK/nvfw, and on an upgrade
# the new location does not exist yet, so a backup that only looked there saved nothing and the
# boot-guard fallback restored the previous driver with no firmware at all - a dark Mac, no recovery.
check('the firmware backup falls back to the old location on an upgrade',
      'then backup_copy firmware ditto "$FWOLD" "$BK/nvfw"' in inst and 'elif [ -d "$FWOLD" ] && [ ! -L "$FWOLD" ]' in inst)

# Kernel review, 2.7.2. The registry is readable by every process, so a kernel pointer there defeats address randomisation.
nvrmfb = (root / 'kexts/NVRMFB/fb/nvrm-fb.cpp').read_text()
nvrmfb2 = (root / 'kexts/NVRM/fb/nvrm-fb.cpp').read_text()
intel = (root / 'kexts/NMIntelFB/intelfb.cpp').read_text()
for name, text in (('NVRM', nvrm), ('NVRMFB', nvrmfb), ('NVRM fb', nvrmfb2), ('NVRM accel', accel), ('NMIntelFB', intel)):
    check(f'{name} neither publishes nor reads a kernel pointer as a registry property',
          '"nvkms-kapi"' not in text and '"phys-for-va"' not in text)
check('both display kexts ask NVRM for the pointers through a kernel-only call',
      all('NVRM_DISPLAY_PTRS_FN' in t for t in (nvrmfb, nvrmfb2, accel)) and 'isEqualTo(NVRM_DISPLAY_PTRS_FN)' in nvrm)
abi1 = (root / 'kexts/NVRM/nvrm_gpuva_abi.h').read_text()
abi2 = (root / 'kexts/NVRMFB/nvrm_gpuva_abi.h').read_text()
check('the two copies of the display ABI header agree', abi1 == abi2)

def body(text, start, end):
    i = text.find(start)
    j = text.find(end, i + len(start)) if i >= 0 else -1
    return text[i:j] if i >= 0 and j >= 0 else ''

bind = body(nvrm, 'IOReturn NVRMUserClient::sBindFd(', 'IOReturn NVRMUserClient::')
def bind_locked(b):
    return (b.find('IOLockLock(me->fMapLock)') >= 0 and
            b.find('IOLockLock(me->fMapLock)') < b.find('nvMapListFree') < b.find('IOLockUnlock(me->fMapLock)'))
check('a bind clears the mapping context only under the mapping lock', bind_locked(bind))
check('negative control: a bind that frees the list after the unlock is caught',
      not bind_locked(bind.replace('        IOLockUnlock(me->fMapLock);\n', '', 1).replace(
          '        IOLockLock(me->fMapLock);\n', '        IOLockLock(me->fMapLock);\n        IOLockUnlock(me->fMapLock);\n', 1)))
cmft = body(nvrm, 'IOReturn NVRMUserClient::clientMemoryForType(', 'IOReturn NVRMUserClient::externalMethod(')
copied = cmft.find('snapshot.memArea.pRanges = ranges;')
# After the copy the live list may only be touched inside the mapping lock: token mode (selector 7) consumes the node it
# copied, after proving under the lock that it is still the same node of the same generation.
def live_list_only_under_lock(post):
    i = 0
    while True:
        i = post.find('fMapList', i)
        if i < 0: return True
        lk, ul = post.rfind('IOLockLock(fMapLock);', 0, i), post.rfind('IOLockUnlock(fMapLock);', 0, i)
        if lk < 0 or ul > lk: return False
        i += 8
post = cmft[copied:]
check('a map reads the live context only under the mapping lock, then only its private copy',
      copied > 0 and 'nvu_memcpy(ranges,' in cmft[:copied] and 'fMmap' not in post
      and live_list_only_under_lock(post)
      and ('fMapList' not in post or 'if (fMapList != snapNode || fMapGen != snapGen) r = kIOReturnBusy;' in post)
      and 'c = &snapshot' in post and 'c = &fMmap' not in cmft)
check('a map refuses an empty range list before reading its first entry',
      'count == 0 || count > 4096' in cmft)
close = body(nvrm, 'IOReturn NVRMUserClient::clientClose(', 'void NVRMUserClient::free(')
check('close marks the context dead under the mapping lock',
      'IOLockLock(fMapLock);\n        if (fMapList) fMapList->context.valid = NV_FALSE;' in close)

look = body(nvrm, 'nv_file_private_t *nv_xnu_lookup_fd(', '\n}\n')
check('a looked-up client is retained before the table lock is dropped',
      0 <= look.find('c->retain()') < look.find('IOLockUnlock(gNvClientsLock)'))
check('a client being closed is not handed out', 'c->cleaned()' in look)
check('putting a client releases it', 'void NV_API_CALL nv_put_file_private(void *os_private) { TRC; nv_xnu_put_client(os_private); }' in osx2)
# NVIDIA 620 took the control/device argument out of nv_get_file_private. Each RM caller that needs one kind checks it
# with nv_verify_nvfp_control_or_put (nv.h): it asks nv_get_state_ptr_from_nvfp, and on a NULL state or the wrong
# NV_FLAG_CONTROL it puts the client and NULLs both pointers. Our part is the answer: NULL for a file that is not one of
# our clients, the control state (the only one with NV_FLAG_CONTROL) for a control client. The two escapes that take
# either kind (ALLOC_MEMORY and MAP_MEMORY name the fd a later map goes through, as on Linux) are decided at map time,
# and a context of the other kind fails closed there: a device context on a control fd has no alloc, a system-memory
# context on a device fd has no ranges (the empty-range-list arm above).
def kind_checked(n, o):
    st = body(n, 'nv_state_t *NV_API_CALL nv_get_state_ptr_from_nvfp(', '\n}\n')
    fr = body(n, 'NVRMUserClient *NVRMUserClient::fromNvfp(', '\n}\n')
    md = body(o, 'extern "C" IOMemoryDescriptor *nv_xnu_alloc_md(void *priv, NvU64 *bytes, NvU32 *cache)\n{', '\n}\n')
    cm = body(n, 'IOReturn NVRMUserClient::clientMemoryForType(', 'IOReturn NVRMUserClient::externalMethod(')
    return ('return c ? c->nvState() : NULL;' in st
            and 'if (!p) return NULL;' in fr and "? c : NULL;" in fr
            and 'nv_state_t *NVRMUserClient::nvState() { return fIsCtl ? nv_get_ctl_state() : &fOwner->fNv; }' in n
            and 'nv_xnu_ctl_state.flags |= NV_FLAG_CONTROL;' in o
            and 'if (!a) return NULL;' in md and 'if (!base) return kIOReturnBadArgument;' in cm)
check('the wrong kind of client is released, and the caller gets no pointer to it', kind_checked(nvrm, osx2))
check('negative control: a state answer for a stranger file is caught',
      not kind_checked(nvrm.replace('return c ? c->nvState() : NULL;', 'return c ? c->nvState() : nv_get_ctl_state();'), osx2))
check('negative control: a control-kind map with no allocation that is not refused is caught',
      not kind_checked(nvrm.replace('if (!base) return kIOReturnBadArgument;', ''), osx2))

# 620: RM writes another client's list only between nv_acquire_file_va and nv_release_file_va (os.c
# _osAddMappingContext), and escape.c puts the client it looked up after the call. Acquire takes that client's
# fMapLock before handing the list out; a file that is not ours gets a list that already holds a node, so RM refuses
# and frees its own node. Release unlocks on every path that acquire locked.
def map_list_locked(n):
    acq = body(n, 'nv_alloc_mapping_list_node_t **NV_API_CALL nv_acquire_file_va(', '\n}\n')
    rel = body(n, 'void NV_API_CALL nv_release_file_va(', '\n}\n')
    guard = 'if (!c || !c->mapLock())'
    tail = rel.split(guard + ' return;', 1)[1] if guard + ' return;' in rel else ''
    return (guard + ' { gNvMapBusy = &gNvMapBusyNode; return &gNvMapBusy; }' in acq
            and 0 <= acq.find('IOLockLock(c->mapLock());') < acq.find('return c->mapList();')
            and tail.rstrip().endswith('IOLockUnlock(c->mapLock());') and 'return' not in tail)
check("another client's context is written under that client's lock and every exit puts it",
      map_list_locked(nvrm))
check('negative control: a release that can leave the lock held is caught',
      not map_list_locked(nvrm.replace('        if (drop) drop->release();   // not the last reference',
                                       '        if (!now) return;\n        if (drop) drop->release();   // not the last reference', 1)))
check('negative control: a stranger file handed an empty list is caught',
      not map_list_locked(nvrm.replace('{ gNvMapBusy = &gNvMapBusyNode; return &gNvMapBusy; }', '{ return nullptr; }')))

access = body(osx2, 'NvBool NV_API_CALL os_check_access(', 'NvU32  NV_API_CALL os_get_current_process_flags')
check('GPU profiling and channel priority are for administrators; any other right is refused',
      'RS_ACCESS_PERFMON' in access and 'RS_ACCESS_NICE' in access and 'os_is_administrator()' in access
      and 'NV_FALSE' in access and 'return NV_TRUE' not in access)

print('FAILED' if fails else 'PASS')
sys.exit(1 if fails else 0)
