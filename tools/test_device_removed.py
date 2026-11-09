#!/usr/bin/env python3
"""After the Vulkan device is lost, a command buffer is still handed out: it records nothing and completes with a
device-removed error, instead of nil (WindowServer asserts on nil and the session goes). Source-level arms, because the
backend cannot be linked on a host: the guarantees are structural. Run: python3 tools/test_device_removed.py [root]"""
import pathlib, re, sys
root = pathlib.Path(sys.argv[1] if len(sys.argv) > 1 else pathlib.Path(__file__).resolve().parents[1])
vk = (root / 'plugin/nvmtl_vk.c').read_text()
ev = (root / 'plugin/NVMTLEventSync.m').read_text()
fails = 0
def check(name, ok):
    global fails
    print(('ok   ' if ok else 'FAIL ') + name); fails += not ok

def func(src, head):
    i = src.find(head)
    return src[i:src.index('\n}\n', i)] if i >= 0 else ''

begin = func(vk, 'int nvmtl_vk_cmd_begin(nvk_cmdbuf *q')
begin = func(vk, 'int nvmtl_vk_cmd_begin(nvk_queue *q, nvk_cmdbuf *out)')
check('a command buffer made after the device was lost is returned, not refused',
      'g_lost_after_up' in begin and 'out->dead = 1' in begin and begin.index('g_lost_after_up') < begin.index('out->dead = 1') < begin.index('const uint64_t b0'))
check('a command buffer made when Vulkan never started is still refused', 'NVMTL_REFUSE_BACKEND_DOWN' in begin)
sb = func(vk, 'int nvmtl_vk_submit_begin_at(')
check('submitting a dead command buffer answers -3 before the backend-down answer',
      'c->dead) return -3' in sb and sb.index('return -3') < sb.index('return -2'))
check('the event segment turns -3 into MTLCommandBufferErrorDeviceRemoved (11)', 'result == -3' in ev and 'code:11' in ev)
check('a dead command buffer is never quarantined (nothing of it is in flight)', 'result == -3' in ev and 'quarantined_parts addObject' in ev and ev.count('result == -3') == 1)
# every recording entry point is a no-op after the loss
stub = func(vk, 'static void nvmtl_vk_stub_recording(void)')
listed = set(re.findall(r'if \(pvk(Cmd\w+)\) pvkCmd\w+ = ', stub))
declared = set(re.findall(r'^static PFN_vkCmd(\w+) pvkCmd\1;', vk, re.M))
loaded = set(re.findall(r'X\(vkCmd(\w+)\)', vk[vk.index('#define DEVICE_FNS'):vk.index('INSTANCE_FNS(DECL)')]))
extra = {d for d in declared if d not in loaded}
missing = sorted(e for e in extra if 'Cmd' + e not in listed)
check('every recording function pointer outside the loaded list is stubbed (missing: %s)' % (missing or 'none'), not missing)
check('the loaded list is stubbed by name prefix', 'strncmp(#f, "vkCmd", 5)' in stub)
check('the loss line names the cause', 'nvmtl_loss_cause(' in func(vk, 'static void nvmtl_vk_note_loss('))
check('both loss sites name the cause', vk.count('nvmtl_vk_note_loss("') == 2)
sys.exit(1 if fails else 0)
