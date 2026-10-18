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
check('bind-fd rejects out-of-range values and clears a stale context when bound to a real fd',
      'if (v > 0x7FFFFFFFLL || v < -0x80000000LL) return kIOReturnBadArgument;' in nvrm and 'if (me->fBound >= 0 && me->fMmap.valid)' in nvrm)

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
check('the mapping ranges are released in free(), not in clientClose',
      'void NVRMUserClient::free(void)' in nvrm
      and 'kern_os_free(fMmap.memArea.pRanges)' in nvrm.split('void NVRMUserClient::free(void)')[1]
      and 'kern_os_free(fMmap.memArea.pRanges)' not in nvrm.split('IOReturn NVRMUserClient::clientClose')[1].split('terminate();')[0])

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
check('the external encoder is off unless a conf knob names the process',
      bool(_nvcvt) and 'getenv("NVMTL_NVENCVT")' in _nvcvt and 'return;' in _nvcvt)
check('the external encoder stays out of WindowServer, like the other codec registrars',
      bool(_nvcvt) and 'strcmp(me, "WindowServer")' in _nvcvt)

# Moving the firmware broke the way out. Both restore paths are gated on $BK/nvfw, and on an upgrade
# the new location does not exist yet, so a backup that only looked there saved nothing and the
# boot-guard fallback restored the previous driver with no firmware at all - a dark Mac, no recovery.
check('the firmware backup falls back to the old location on an upgrade',
      'elif [ -d "$FWOLD" ]; then backup_copy firmware ditto "$FWOLD" "$BK/nvfw"' in inst)

print('FAILED' if fails else 'PASS')
sys.exit(1 if fails else 0)
