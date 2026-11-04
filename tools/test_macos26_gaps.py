#!/usr/bin/env python3
"""Source checks for the macOS 26 Metal 3 additions: required threadgroup getters, depth bounds, texture views by
descriptor, and the Metal 3 colour attachment map. No GPU needed; the pixel behaviour is tools/oracles/macos26_gaps."""
import pathlib
import re
import sys

root = pathlib.Path(__file__).resolve().parents[1]
objects = (root / 'plugin/NVMTLObjects.m').read_text()
header = (root / 'plugin/NVMTLObjects.h').read_text()
gl = (root / 'plugin/NVMTLGL.m').read_text()
device = (root / 'plugin/NVMTLDevice.m').read_text()
library = (root / 'plugin/NVMTLLibraryLoad.m').read_text()
vk = (root / 'plugin/nvmtl_vk.c').read_text()
vkh = (root / 'plugin/nvmtl_vk.h').read_text()

failed = 0


def arm(name, ok, detail=''):
    global failed
    if not ok:
        failed += 1
    print(('ok    ' if ok else 'FAIL  ') + name + (f'  ({detail})' if detail else ''))


def body(src, signature):
    """The method body that follows signature, up to its closing brace at column 0 or the next method."""
    i = src.find(signature)
    if i < 0:
        return None
    ends = [x for x in (src.find('\n- (', i + 1), src.find('\n@end', i + 1), src.find('\n}\n', i + 1)) if x > 0]
    return src[i:min(ends) if ends else len(src)]


# 1 · required threadgroup sizes
arm('compute pipeline defines requiredThreadsPerThreadgroup', '- (MTLSize)requiredThreadsPerThreadgroup { return _reqTG; }' in objects)
for g, iv in (('Tile', None), ('Object', '_reqObj'), ('Mesh', '_reqMesh')):
    sig = f'- (MTLSize)requiredThreadsPer{g}Threadgroup'
    arm(f'render pipeline defines requiredThreadsPer{g}Threadgroup', sig in objects)
    if iv:
        arm(f'the {g.lower()} getter returns what the mesh descriptor gave', f'{sig} {{ return {iv}; }}' in objects and f'ps->{iv} = ' in library)
arm('the compute descriptor value is stored on the pipeline',
    "->_reqTG = nvmtl_desc_size(d, @selector(requiredThreadsPerThreadgroup));" in device)
mt = body(objects, '- (NSUInteger)maxTotalThreadsPerThreadgroup { NSUInteger v = nvmtl_compute_maxt()')
arm('a required size caps maxTotalThreadsPerThreadgroup', mt is not None and '_reqTG.width * _reqTG.height * _reqTG.depth' in mt)
arm('descriptor sizes are read by key, so a macOS 15 build needs no availability check',
    'static inline MTLSize nvmtl_desc_size(id d, SEL sel)' in header and 'valueForKey:NSStringFromSelector(sel)' in header)

# 2 · depth bounds
arm('the empty setDepthTestMinBound:maxBound: stub is gone', 'setDepthTestMinBound' not in gl)
db = body(objects, '- (void)setDepthTestMinBound:(float)lo maxBound:(float)hi {')
arm('setDepthTestMinBound:maxBound: stores the range and turns the test on',
    db is not None and '_dbOn = YES; _dbLo = lo; _dbHi = hi;' in db and 'nvmtlApplyDepthStencil' in db)
ap = body(objects, '- (void)nvmtlApplyDepthStencil {')
arm('the encoder writes the depth bounds state at pass begin', ap is not None and 'nvmtl_vk_cmd_set_depth_bounds(&_cb->_c, _dbOn, _dbLo, _dbHi)' in ap)
arm('the depthBounds feature is enabled when the device has it',
    'enabled.depthBounds = supported.depthBounds; g_depth_bounds = supported.depthBounds == VK_TRUE;' in vk)
arm('depth pipelines carry the bounds test and range as dynamic state',
    'dyn[ndyn++] = VK_DYNAMIC_STATE_DEPTH_BOUNDS_TEST_ENABLE; dyn[ndyn++] = VK_DYNAMIC_STATE_DEPTH_BOUNDS;' in vk
    and 'out->db_dyn = hasDepth && g_depth_bounds;' in vk)
m = re.search(r'VkDynamicState dyn\[(\d+)\] = \{(.*?)\};', vk, re.S)
base = len(re.findall(r'VK_DYNAMIC_STATE_\w+', m.group(2))) if m else 0
# fixed list + bias enable + 3 dynamic-state3 + bounds 2 + stride + line width + sample locations
worst = base + 1 + 3 + 2 + 1 + 1 + 1
arm('the dynamic state array holds the worst case', m is not None and int(m.group(1)) >= worst, f'{m.group(1) if m else "?"} slots, worst {worst}')
bp = body(vk, 'void nvmtl_vk_cmd_bind_pipeline(nvk_cmdbuf *c, nvk_pipeline *p) {') or ''
arm('binding a pipeline re-emits the encoder bounds', 'if (p->db_dyn)' in bp and 'pvkCmdSetDepthBounds(' in bp)
arm('both Vulkan entry points are loaded', 'X(vkCmdSetDepthBoundsTestEnable) X(vkCmdSetDepthBounds)' in vk)
arm('the setter refuses without the feature instead of recording an invalid command',
    'if (!g_depth_bounds) return on ? -1 : 0;' in vk and 'int  nvmtl_vk_cmd_set_depth_bounds(' in vkh)

# 3 · newTextureViewWithDescriptor:
tv = body(objects, '- (id<MTLTexture>)newTextureViewWithDescriptor:(id)d {')
arm('texture defines newTextureViewWithDescriptor:', tv is not None)
arm('the descriptor view goes through the swizzle view path',
    tv is not None and 'newTextureViewWithPixelFormat:f textureType:tt levels:levels slices:slices swizzle:sw' in tv)
for key in ('pixelFormat', 'textureType', 'levelRange', 'sliceRange', 'swizzle'):
    arm(f'the descriptor field {key} is read', tv is not None and f'valueForKey:@"{key}"' in tv)

# 4 · colour attachment map
cm = body(objects, '- (void)setColorAttachmentMap:(id)map {')
arm('Metal 3 render encoder defines setColorAttachmentMap:', cm is not None)

print(f'RESULT {failed} failed')
sys.exit(1 if failed else 0)
