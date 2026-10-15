#!/usr/bin/env python3
"""The copy-engine graphics-channel retry records its outcome where Send logs can read it.

Non-Turing cards log "chandesConstruct_IMPL: bad class 0xc7b5" because the first copy object is allocated on a graphics
channel; the partner-copy-engine retry then names the engine. To learn, from the field, whether that retry succeeds (and
on the chips where it does not, with what partner count and RM status), the retry must leave a line in a log Send logs
collects. Send logs reads nvmtl.log (nullmoth-setup.sh: "== plugin file log" from nvmtl_log_path), and the plugin's
Vulkan debug messenger writes NVK's warnings there tagged "nvk-reason:" (plugin/nvmtl_vk.c). So the retry must emit
through vk_logw/vk_loge (which reach that messenger), not fprintf(stderr), which reached neither nvmtl.log nor the
kernel log.

Negative control: the previous code logged this with fprintf(stderr) and recorded no chosen engine or retry status, so
the test fails on the release-2.0 source.
"""
import pathlib
import re
import sys

root = pathlib.Path(sys.argv[1] if len(sys.argv) > 1 else pathlib.Path(__file__).resolve().parents[1])
patch = (root / 'nvk/nvk-macos.patch').read_text()
fails = 0


def check(name, ok, detail=''):
    global fails
    print(('ok   ' if ok else 'FAIL ') + name + (f' ({detail})' if detail else ''))
    fails += not ok


# Isolate the graphics-channel copy retry: from the partner-list lookup call to the following NV_CHECK(cs).
# Anchor on the control CALL (nvRmApiControl ... PARTNERLIST), not the header mention of the same token in a comment.
lo = patch.find('nvRmApiControl(&rm, pdev->hSubdevice, NV2080_CTRL_CMD_GPU_GET_ENGINE_PARTNERLIST')
hi = patch.find('NV_CHECK(cs);', lo) if lo >= 0 else -1
check('the partner-copy-engine retry block is present', lo >= 0 and hi > lo)
block = patch[lo:hi] if hi > lo else ''
# Strip the unified-diff '+' column so this reads the C, not the diff.
code = '\n'.join(l[1:] if l[:1] == '+' else l for l in block.split('\n'))

# 1. the outcome is emitted through the Vulkan debug messenger (vk_logw/vk_loge), which the plugin forwards to
#    nvmtl.log; the messenger subscribes to WARNING and ERROR only, so vk_logi/vk_logd would be dropped.
emit = re.search(r'vk_log([we])\s*\(\s*VK_LOG_OBJS\s*\(\s*log_obj\s*\)', code)
check('the retry emits via vk_logw/vk_loge(VK_LOG_OBJS(log_obj)) (reaches the nvk-reason messenger)', bool(emit),
      'severity ' + (emit.group(1) if emit else 'none'))

# 2. the line records partner count, the engine chosen, and the retry status (and the first-attempt status).
# The format string itself contains ';', so take the whole emission (its args run to the end of this scoped block).
msg = code[emit.end():] if emit else ''
for field, token in [('partner count', 'pl.numPartners'), ('engine chosen', 'ceEngine'),
                     ('retry status', ')cs'), ('first-attempt status', 'first')]:
    check(f'the logged line carries the {field}', token in msg.replace(' ', ''), token)

# 3. the retry no longer loses this to stderr (the old path Send logs never saw).
check('the retry does not use fprintf(stderr) for this evidence', 'fprintf(stderr' not in code)

# 4. cross-check the collector: the plugin messenger writes "nvk-reason:" to nvmtl_log_path, and setup collects it.
vk = (root / 'plugin/nvmtl_vk.c').read_text()
check('the plugin messenger tags NVK warnings "nvk-reason:" and writes nvmtl_log_path',
      'nvk-reason:' in vk and 'nvmtl_log_path()' in vk)
check('the messenger subscribes to WARNING severity (so vk_logw is delivered)',
      'VK_DEBUG_UTILS_MESSAGE_SEVERITY_WARNING_BIT_EXT' in vk)
setup = (root / 'app/Resources/nullmoth-setup.sh').read_text()
check('Send logs collects nvmtl.log into driver-plugin-log.txt (no capture-script change needed)',
      'nvmtl.log' in setup and 'driver-plugin-log.txt' in setup)

print('FAILED' if fails else 'PASS')
sys.exit(1 if fails else 0)
