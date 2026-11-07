/*
 * NullMoth NVIDIA driver for macOS
 * Copyright (c) 2026 NullMoth Systems.
 * SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0
 */

// A MetalFX temporal scaler that upscales with DLSS Super Resolution (nvmtl_dlss.h). It is handed out by the
// descriptor's newTemporalScalerWithDevice: hook in nvmtl_mfx_native_profile.inc, only for this driver's devices and only
// while DLSS is ready; every other case keeps the existing MetalFX path.
//
// How MetalFX settings reach NGX (descriptor = creation, scaler property = per frame):
//   descriptor inputWidth/Height, outputWidth/Height   -> Width/Height, OutWidth/OutHeight; PerfQualityValue from out/in
//   descriptor colorTextureFormat float (RGBA16F, RG11B10F, RGB9E5) -> create flag IsHDR
//   descriptor outputResolutionMotionVectorsEnabled off -> create flag MVLowRes (motion vectors at input size)
//   descriptor jitteredMotionVectorsEnabled            -> create flag MVJittered
//   descriptor autoExposureEnabled                     -> create flag AutoExposure (otherwise exposureTexture is passed)
//   scaler depthReversed                               -> create flag DepthInverted (a change makes a new feature)
//   scaler colorTexture/depthTexture/motionTexture/outputTexture/exposureTexture/reactiveMaskTexture
//                                                      -> Color/Depth/MotionVectors/Output/ExposureTexture/bias mask
//   scaler jitterOffsetX/Y, motionVectorScaleX/Y, reset -> Jitter.Offset.X/Y, MV.Scale.X/Y, Reset
//   scaler preExposure                                 -> DLSS.Pre.Exposure
//   scaler inputContentWidth/Height                    -> DLSS.Render.Subrect.Dimensions
//   scaler color/depth/motion content offsets          -> DLSS.Input.{Color,Depth,MV}.Subrect.Base
// Not mapped: output offsets (the feature is made without output sub-rectangles), the fence property (the scaler is
// encoded in order on the same command buffer, so no wait is recorded), inputContentMinScale/MaxScale beyond reporting
// them, and the frame-interpolation side of the protocol.
#import <Foundation/Foundation.h>
#import <Metal/Metal.h>
#import <MetalFX/MetalFX.h>

@interface NVMTLDLSSScaler : NSObject <MTLFXTemporalScaler> {
    void *_feature;   // nvmtl_dlss_feature, made on the first encode
    BOOL _hdr, _mvLowRes, _mvJittered, _autoExposure;
    NSUInteger _inW, _inH, _outW, _outH;
    MTLPixelFormat _fmtColor, _fmtDepth, _fmtMotion, _fmtReactive, _fmtOutput;
    BOOL _reactiveEnabled;
    float _minScale, _maxScale;
    BOOL _notedUnmapped;
#if __MAC_OS_X_VERSION_MAX_ALLOWED < 270000
    NSUInteger _colorContentOffsetX, _colorContentOffsetY, _depthContentOffsetX, _depthContentOffsetY;
    NSUInteger _motionContentOffsetX, _motionContentOffsetY, _reactiveMaskContentOffsetX, _reactiveMaskContentOffsetY;
    NSUInteger _outputOffsetX, _outputOffsetY;
#endif
}
- (instancetype)initWithDescriptor:(MTLFXTemporalScalerDescriptor *)d;
@end

@implementation NVMTLDLSSScaler
@synthesize colorTexture = _colorTexture, depthTexture = _depthTexture, motionTexture = _motionTexture, outputTexture = _outputTexture;
@synthesize exposureTexture = _exposureTexture, reactiveMaskTexture = _reactiveMaskTexture, fence = _fence;
@synthesize preExposure = _preExposure, jitterOffsetX = _jitterOffsetX, jitterOffsetY = _jitterOffsetY;
@synthesize motionVectorScaleX = _motionVectorScaleX, motionVectorScaleY = _motionVectorScaleY, reset = _reset, depthReversed = _depthReversed;
@synthesize inputContentWidth = _inputContentWidth, inputContentHeight = _inputContentHeight;
// The content and output offsets exist in the protocol only in the newest SDK; the ivars are always there.
#if __MAC_OS_X_VERSION_MAX_ALLOWED >= 270000
@synthesize colorContentOffsetX = _colorContentOffsetX, colorContentOffsetY = _colorContentOffsetY;
@synthesize depthContentOffsetX = _depthContentOffsetX, depthContentOffsetY = _depthContentOffsetY;
@synthesize motionContentOffsetX = _motionContentOffsetX, motionContentOffsetY = _motionContentOffsetY;
@synthesize reactiveMaskContentOffsetX = _reactiveMaskContentOffsetX, reactiveMaskContentOffsetY = _reactiveMaskContentOffsetY;
@synthesize outputOffsetX = _outputOffsetX, outputOffsetY = _outputOffsetY;
#endif

static BOOL nvmtl_dlss_desc_flag(id d, SEL s) {
    return [d respondsToSelector:s] ? ((BOOL (*)(id, SEL))objc_msgSend)(d, s) : NO;   // properties newer than the OS this runs on
}
static BOOL nvmtl_dlss_float_format(MTLPixelFormat f) {
    return f == MTLPixelFormatRGBA16Float || f == MTLPixelFormatRG11B10Float || f == MTLPixelFormatRGB9E5Float || f == MTLPixelFormatRGBA32Float;
}

- (instancetype)initWithDescriptor:(MTLFXTemporalScalerDescriptor *)d {
    if (!(self = [super init])) return nil;
    _inW = d.inputWidth; _inH = d.inputHeight; _outW = d.outputWidth; _outH = d.outputHeight;
    _fmtColor = d.colorTextureFormat; _fmtDepth = d.depthTextureFormat; _fmtMotion = d.motionTextureFormat; _fmtOutput = d.outputTextureFormat;
    _reactiveEnabled = d.reactiveMaskTextureEnabled; _fmtReactive = _reactiveEnabled ? d.reactiveMaskTextureFormat : MTLPixelFormatInvalid;
    _hdr = nvmtl_dlss_float_format(_fmtColor);
    _mvLowRes = !nvmtl_dlss_desc_flag(d, @selector(isOutputResolutionMotionVectorsEnabled));
    _mvJittered = nvmtl_dlss_desc_flag(d, @selector(isJitteredMotionVectorsEnabled));
    _autoExposure = d.autoExposureEnabled;
    _minScale = d.inputContentMinScale; _maxScale = d.inputContentMaxScale;
    _preExposure = 1.0f; _motionVectorScaleX = 1.0f; _motionVectorScaleY = 1.0f;
    _inputContentWidth = _inW; _inputContentHeight = _inH;
    return self;
}
- (MTLTextureUsage)colorTextureUsage { return MTLTextureUsageShaderRead; }
- (MTLTextureUsage)depthTextureUsage { return MTLTextureUsageShaderRead; }
- (MTLTextureUsage)motionTextureUsage { return MTLTextureUsageShaderRead; }
- (MTLTextureUsage)reactiveMaskTextureUsage { return MTLTextureUsageShaderRead; }
- (MTLTextureUsage)reactiveTextureUsage { return MTLTextureUsageShaderRead; }
- (MTLTextureUsage)outputTextureUsage { return MTLTextureUsageShaderRead | MTLTextureUsageShaderWrite; }
- (MTLPixelFormat)colorTextureFormat { return _fmtColor; }
- (MTLPixelFormat)depthTextureFormat { return _fmtDepth; }
- (MTLPixelFormat)motionTextureFormat { return _fmtMotion; }
- (MTLPixelFormat)reactiveMaskTextureFormat { return _fmtReactive; }
- (MTLPixelFormat)outputTextureFormat { return _fmtOutput; }
- (NSUInteger)inputWidth { return _inW; }
- (NSUInteger)inputHeight { return _inH; }
- (NSUInteger)outputWidth { return _outW; }
- (NSUInteger)outputHeight { return _outH; }
- (float)inputContentMinScale { return _minScale; }
- (float)inputContentMaxScale { return _maxScale; }

static NVMTLTexture *nvmtl_dlss_tex(id<MTLTexture> t) { return [(id)t isKindOfClass:[NVMTLTexture class]] ? (NVMTLTexture *)t : nil; }

- (void)encodeToCommandBuffer:(id<MTLCommandBuffer>)commandBuffer {
    if (![(id)commandBuffer isKindOfClass:[NVMTLCommandBuffer class]]) { nvlog("dlss: the command buffer is not this driver's - upscale skipped"); return; }
    NVMTLCommandBuffer *cb = (NVMTLCommandBuffer *)commandBuffer;
    NVMTLTexture *color = nvmtl_dlss_tex(_colorTexture), *depth = nvmtl_dlss_tex(_depthTexture), *motion = nvmtl_dlss_tex(_motionTexture), *out = nvmtl_dlss_tex(_outputTexture);
    NVMTLTexture *expo = nvmtl_dlss_tex(_exposureTexture), *react = nvmtl_dlss_tex(_reactiveMaskTexture);
    if (!color || !depth || !motion || !out) { nvlog("dlss: colour, depth, motion and output textures must all be set (and be this driver's) - upscale skipped"); return; }
    if (!_autoExposure && _exposureTexture && !expo) { nvlog("dlss: the exposure texture is not this driver's - upscale skipped"); return; }
    if (_reactiveMaskTexture && !react) { nvlog("dlss: the reactive mask is not this driver's - upscale skipped"); return; }
    if (!_notedUnmapped) {
        _notedUnmapped = YES;
        if (_outputOffsetX || _outputOffsetY || _fence) nvlog("dlss: output offsets and the fence property are not mapped to NGX (ignored)");
    }
    nvmtl_vk_dlss_args a = {0};
    a.color = [color nvi]; a.color_view = [color nvview]; a.depth = [depth nvi]; a.depth_view = [depth nvview];
    a.motion = [motion nvi]; a.motion_view = [motion nvview]; a.output = [out nvi]; a.output_view = [out nvview];
    if (expo && !_autoExposure) { a.exposure = [expo nvi]; a.exposure_view = [expo nvview]; }
    if (react) { a.reactive = [react nvi]; a.reactive_view = [react nvview]; }
    a.in_w = (uint32_t)_inW; a.in_h = (uint32_t)_inH; a.out_w = (uint32_t)_outW; a.out_h = (uint32_t)_outH;
    a.cfg_in_w_used = (uint32_t)(_inputContentWidth ? _inputContentWidth : _inW); a.cfg_in_h_used = (uint32_t)(_inputContentHeight ? _inputContentHeight : _inH);
    a.hdr = _hdr; a.mv_lowres = _mvLowRes; a.mv_jittered = _mvJittered; a.depth_inverted = _depthReversed; a.auto_exposure = _autoExposure || !a.exposure;
    a.jitter_x = _jitterOffsetX; a.jitter_y = _jitterOffsetY; a.mv_scale_x = _motionVectorScaleX; a.mv_scale_y = _motionVectorScaleY;
    a.pre_exposure = _preExposure; a.reset = _reset;
    a.color_x = (uint32_t)_colorContentOffsetX; a.color_y = (uint32_t)_colorContentOffsetY;
    a.depth_x = (uint32_t)_depthContentOffsetX; a.depth_y = (uint32_t)_depthContentOffsetY;
    a.motion_x = (uint32_t)_motionContentOffsetX; a.motion_y = (uint32_t)_motionContentOffsetY;
    int rc;
    @synchronized (self) { rc = nvmtl_vk_cmd_dlss(&cb->_c, &_feature, &a); }
    if (rc) { nvlog("dlss: upscale was not recorded (output left as it was)"); return; }
    nvmtl_retain_resource(cb, color); nvmtl_retain_resource(cb, depth); nvmtl_retain_resource(cb, motion); nvmtl_retain_resource(cb, out);
    if (expo && !_autoExposure) nvmtl_retain_resource(cb, expo);
    if (react) nvmtl_retain_resource(cb, react);
    NVMTLTexture *rs = out->_parent ? out->_parent : out;
    if (rs->_surf) nvmtl_surf_dirty(cb, rs);
}
@end

// Called from the descriptor hooks (nvmtl_mfx_native_profile.inc).
static BOOL nvmtl_mfx_dlss_ready(void) { return nvmtl_vk_dlss_available(); }
static id nvmtl_mfx_dlss_make(id descriptor, id device) {
    if (!nvmtl_vk_dlss_available()) return nil;
    MTLFXTemporalScalerDescriptor *d = (MTLFXTemporalScalerDescriptor *)descriptor;
    if (!d.inputWidth || !d.inputHeight || !d.outputWidth || !d.outputHeight || d.outputWidth < d.inputWidth || d.outputHeight < d.inputHeight) {
        nvlog("dlss: scaler %lux%lu -> %lux%lu is not an upscale - using the MetalFX path", (unsigned long)d.inputWidth, (unsigned long)d.inputHeight,
              (unsigned long)d.outputWidth, (unsigned long)d.outputHeight);
        return nil;
    }
    if ((double)d.outputWidth > 3.01 * (double)d.inputWidth || (double)d.outputHeight > 3.01 * (double)d.inputHeight) {
        nvlog("dlss: scaler ratio is beyond 3x - using the MetalFX path");
        return nil;
    }
    NVMTLDLSSScaler *s = [[NVMTLDLSSScaler alloc] initWithDescriptor:d];
    nvlog("dlss: MetalFX temporal scaler %lux%lu -> %lux%lu served by DLSS", (unsigned long)d.inputWidth, (unsigned long)d.inputHeight,
          (unsigned long)d.outputWidth, (unsigned long)d.outputHeight);
    return s;
}
