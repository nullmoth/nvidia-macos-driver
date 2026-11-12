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

// The command buffer DLSS records into. A Metal 3 encode passes this driver's NVMTLCommandBuffer; a Metal 4 encode passes
// an MTL4 command buffer, which hands out the recording it is filling through -nvmtl_recordingCommandBuffer (between
// beginCommandBufferWithAllocator: and endCommandBuffer). Anything else is not this driver's.
static NVMTLCommandBuffer *nvmtl_dlss_recording_cb(id commandBuffer) {
    if ([commandBuffer isKindOfClass:[NVMTLCommandBuffer class]]) return (NVMTLCommandBuffer *)commandBuffer;
    SEL rec = sel_registerName("nvmtl_recordingCommandBuffer");
    if ([commandBuffer respondsToSelector:rec]) {
        id r = ((id (*)(id, SEL))objc_msgSend)(commandBuffer, rec);
        if ([r isKindOfClass:[NVMTLCommandBuffer class]]) return (NVMTLCommandBuffer *)r;
    }
    return nil;
}
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
    _reactiveEnabled = NVMTL_SINCE(14.4, BOOL, d.reactiveMaskTextureEnabled, NO); _fmtReactive = _reactiveEnabled ? NVMTL_SINCE(14.4, MTLPixelFormat, d.reactiveMaskTextureFormat, MTLPixelFormatInvalid) : MTLPixelFormatInvalid;
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
    NVMTLCommandBuffer *cb = nvmtl_dlss_recording_cb(commandBuffer);
    if (!cb) { nvlog("dlss: the command buffer is not this driver's - upscale skipped"); return; }
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

// The denoised scaler is macOS 26 API; the hooks that hand this class out are installed only when the class exists at run time.
#pragma clang diagnostic push
#pragma clang diagnostic ignored "-Wunguarded-availability-new"
// ---- Ray Reconstruction behind MTLFXTemporalDenoisedScaler ------------------------------------------------------------
// Handed out by the denoised descriptor's newTemporalDenoisedScalerWithDevice: hook (nvmtl_mfx_native_profile.inc) only for
// this driver's devices and only while the NGX core reports Ray Reconstruction. Mapping to NGX:
//   colorTexture/depthTexture/motionTexture/outputTexture -> Color/Depth/MotionVectors/Output
//   diffuseAlbedoTexture, specularAlbedoTexture            -> DLSS.Input.DiffuseAlbedo, DLSS.Input.SpecularAlbedo
//   normalTexture, roughnessTexture                        -> GBuffer.Normals, GBuffer.Roughness (roughness in its own texture)
//   specularHitDistanceTexture (optional)                  -> DLSSD.SpecularHitDistance
//   worldToViewMatrix, viewToClipMatrix                    -> WorldToViewMatrix, ViewToClipMatrix
//   exposure, preExposure, reactiveMask, jitter, motion scale, shouldResetHistory, depthReversed as for the temporal scaler
// Not mapped (logged once): denoiseStrengthMaskTexture and transparencyOverlayTexture (NGX takes a transparency layer as
// separate colour, opacity and motion textures), the fence, and motion vectors that include jitter.
@interface NVMTLDLSSDenoisedScaler : NSObject <MTLFXTemporalDenoisedScaler> {
    void *_feature;   // nvmtl_dlss_rr_feature, made on the first encode
    BOOL _hdr, _autoExposure, _reactiveEnabled, _hitEnabled, _notedUnmapped;
    NSUInteger _inW, _inH, _outW, _outH;
    MTLPixelFormat _fmt[12];
}
- (instancetype)initWithDescriptor:(MTLFXTemporalDenoisedScalerDescriptor *)d;
@end

@implementation NVMTLDLSSDenoisedScaler
@synthesize colorTexture = _colorTexture, depthTexture = _depthTexture, motionTexture = _motionTexture, outputTexture = _outputTexture;
@synthesize diffuseAlbedoTexture = _diffuseAlbedoTexture, specularAlbedoTexture = _specularAlbedoTexture;
@synthesize normalTexture = _normalTexture, roughnessTexture = _roughnessTexture, specularHitDistanceTexture = _specularHitDistanceTexture;
@synthesize denoiseStrengthMaskTexture = _denoiseStrengthMaskTexture, transparencyOverlayTexture = _transparencyOverlayTexture;
@synthesize exposureTexture = _exposureTexture, reactiveMaskTexture = _reactiveMaskTexture, fence = _fence;
@synthesize preExposure = _preExposure, jitterOffsetX = _jitterOffsetX, jitterOffsetY = _jitterOffsetY;
@synthesize motionVectorScaleX = _motionVectorScaleX, motionVectorScaleY = _motionVectorScaleY;
@synthesize shouldResetHistory = _shouldResetHistory, depthReversed = _depthReversed;
@synthesize worldToViewMatrix = _worldToViewMatrix, viewToClipMatrix = _viewToClipMatrix;
enum { RF_COLOR, RF_DEPTH, RF_MOTION, RF_DIFFUSE, RF_SPECULAR, RF_NORMAL, RF_ROUGH, RF_HIT, RF_MASK, RF_TRANSP, RF_REACT, RF_OUT };

- (instancetype)initWithDescriptor:(MTLFXTemporalDenoisedScalerDescriptor *)d {
    if (!(self = [super init])) return nil;
    _inW = d.inputWidth; _inH = d.inputHeight; _outW = d.outputWidth; _outH = d.outputHeight;
    _fmt[RF_COLOR] = d.colorTextureFormat; _fmt[RF_DEPTH] = d.depthTextureFormat; _fmt[RF_MOTION] = d.motionTextureFormat;
    _fmt[RF_DIFFUSE] = d.diffuseAlbedoTextureFormat; _fmt[RF_SPECULAR] = d.specularAlbedoTextureFormat;
    _fmt[RF_NORMAL] = d.normalTextureFormat; _fmt[RF_ROUGH] = d.roughnessTextureFormat; _fmt[RF_HIT] = d.specularHitDistanceTextureFormat;
    _fmt[RF_MASK] = d.denoiseStrengthMaskTextureFormat; _fmt[RF_TRANSP] = d.transparencyOverlayTextureFormat;
    _fmt[RF_REACT] = d.reactiveMaskTextureFormat; _fmt[RF_OUT] = d.outputTextureFormat;
    _hdr = nvmtl_dlss_float_format(_fmt[RF_COLOR]);
    _autoExposure = d.autoExposureEnabled; _reactiveEnabled = d.reactiveMaskTextureEnabled; _hitEnabled = d.specularHitDistanceTextureEnabled;
    _preExposure = 1.0f; _motionVectorScaleX = 1.0f; _motionVectorScaleY = 1.0f;
    return self;
}
- (MTLTextureUsage)colorTextureUsage { return MTLTextureUsageShaderRead; }
- (MTLTextureUsage)depthTextureUsage { return MTLTextureUsageShaderRead; }
- (MTLTextureUsage)motionTextureUsage { return MTLTextureUsageShaderRead; }
- (MTLTextureUsage)reactiveTextureUsage { return MTLTextureUsageShaderRead; }
- (MTLTextureUsage)diffuseAlbedoTextureUsage { return MTLTextureUsageShaderRead; }
- (MTLTextureUsage)specularAlbedoTextureUsage { return MTLTextureUsageShaderRead; }
- (MTLTextureUsage)normalTextureUsage { return MTLTextureUsageShaderRead; }
- (MTLTextureUsage)roughnessTextureUsage { return MTLTextureUsageShaderRead; }
- (MTLTextureUsage)specularHitDistanceTextureUsage { return MTLTextureUsageShaderRead; }
- (MTLTextureUsage)denoiseStrengthMaskTextureUsage { return MTLTextureUsageShaderRead; }
- (MTLTextureUsage)transparencyOverlayTextureUsage { return MTLTextureUsageShaderRead; }
- (MTLTextureUsage)outputTextureUsage { return MTLTextureUsageShaderRead | MTLTextureUsageShaderWrite; }
- (MTLPixelFormat)colorTextureFormat { return _fmt[RF_COLOR]; }
- (MTLPixelFormat)depthTextureFormat { return _fmt[RF_DEPTH]; }
- (MTLPixelFormat)motionTextureFormat { return _fmt[RF_MOTION]; }
- (MTLPixelFormat)diffuseAlbedoTextureFormat { return _fmt[RF_DIFFUSE]; }
- (MTLPixelFormat)specularAlbedoTextureFormat { return _fmt[RF_SPECULAR]; }
- (MTLPixelFormat)normalTextureFormat { return _fmt[RF_NORMAL]; }
- (MTLPixelFormat)roughnessTextureFormat { return _fmt[RF_ROUGH]; }
- (MTLPixelFormat)specularHitDistanceTextureFormat { return _fmt[RF_HIT]; }
- (MTLPixelFormat)denoiseStrengthMaskTextureFormat { return _fmt[RF_MASK]; }
- (MTLPixelFormat)transparencyOverlayTextureFormat { return _fmt[RF_TRANSP]; }
- (MTLPixelFormat)reactiveMaskTextureFormat { return _fmt[RF_REACT]; }
- (MTLPixelFormat)outputTextureFormat { return _fmt[RF_OUT]; }
- (NSUInteger)inputWidth { return _inW; }
- (NSUInteger)inputHeight { return _inH; }
- (NSUInteger)outputWidth { return _outW; }
- (NSUInteger)outputHeight { return _outH; }
- (float)inputContentMinScale { return NVMTL_DLSS_MIN_INPUT_SCALE; }
- (float)inputContentMaxScale { return NVMTL_DLSS_MAX_INPUT_SCALE; }

// simd_float4x4 is column-major; the 16 floats are handed over in that memory order (NVMTL_DLSS_RR_TRANSPOSE=1 flips it
// should a run show the core wants row-major).
static void nvmtl_rr_matrix(float out[16], simd_float4x4 m) {
    const BOOL t = getenv("NVMTL_DLSS_RR_TRANSPOSE") && getenv("NVMTL_DLSS_RR_TRANSPOSE")[0] == '1';
    for (int c = 0; c < 4; c++) for (int r = 0; r < 4; r++) out[t ? r * 4 + c : c * 4 + r] = m.columns[c][r];
}

- (void)encodeToCommandBuffer:(id<MTLCommandBuffer>)commandBuffer {
    NVMTLCommandBuffer *cb = nvmtl_dlss_recording_cb(commandBuffer);
    if (!cb) { nvlog("dlss: RR: the command buffer is not this driver's - denoise skipped"); return; }
    NVMTLTexture *color = nvmtl_dlss_tex(_colorTexture), *depth = nvmtl_dlss_tex(_depthTexture), *motion = nvmtl_dlss_tex(_motionTexture), *out = nvmtl_dlss_tex(_outputTexture);
    NVMTLTexture *diff = nvmtl_dlss_tex(_diffuseAlbedoTexture), *spec = nvmtl_dlss_tex(_specularAlbedoTexture);
    NVMTLTexture *nrm = nvmtl_dlss_tex(_normalTexture), *rgh = nvmtl_dlss_tex(_roughnessTexture), *hit = nvmtl_dlss_tex(_specularHitDistanceTexture);
    NVMTLTexture *expo = nvmtl_dlss_tex(_exposureTexture), *react = nvmtl_dlss_tex(_reactiveMaskTexture);
    if (!color || !depth || !motion || !out || !diff || !spec || !nrm || !rgh) {
        nvlog("dlss: RR: colour, depth, motion, diffuse albedo, specular albedo, normal, roughness and output textures must all be set (and be this driver's) - denoise skipped");
        return;
    }
    if (_specularHitDistanceTexture && !hit) { nvlog("dlss: RR: the hit distance texture is not this driver's - denoise skipped"); return; }
    if (!_autoExposure && _exposureTexture && !expo) { nvlog("dlss: RR: the exposure texture is not this driver's - denoise skipped"); return; }
    if (_reactiveMaskTexture && !react) { nvlog("dlss: RR: the reactive mask is not this driver's - denoise skipped"); return; }
    if (!_notedUnmapped) {
        _notedUnmapped = YES;
        if (_denoiseStrengthMaskTexture || _transparencyOverlayTexture || _fence)
            nvlog("dlss: RR: the denoise strength mask, the transparency overlay and the fence are not mapped to NGX (ignored)");
    }
    nvmtl_vk_dlss_rr_args a = {0};
    a.color = [color nvi]; a.color_view = [color nvview]; a.depth = [depth nvi]; a.depth_view = [depth nvview];
    a.motion = [motion nvi]; a.motion_view = [motion nvview]; a.output = [out nvi]; a.output_view = [out nvview];
    a.diffuse_albedo = [diff nvi]; a.diffuse_albedo_view = [diff nvview]; a.specular_albedo = [spec nvi]; a.specular_albedo_view = [spec nvview];
    a.normals = [nrm nvi]; a.normals_view = [nrm nvview]; a.roughness = [rgh nvi]; a.roughness_view = [rgh nvview];
    if (hit) { a.hit_distance = [hit nvi]; a.hit_distance_view = [hit nvview]; }
    if (expo && !_autoExposure) { a.exposure = [expo nvi]; a.exposure_view = [expo nvview]; }
    if (react) { a.reactive = [react nvi]; a.reactive_view = [react nvview]; }
    a.in_w = (uint32_t)_inW; a.in_h = (uint32_t)_inH; a.out_w = (uint32_t)_outW; a.out_h = (uint32_t)_outH;
    // The denoised descriptor has no motion-resolution flag: motion vectors are taken at the input size, without jitter.
    a.hdr = _hdr; a.mv_lowres = 1; a.mv_jittered = 0; a.depth_inverted = _depthReversed; a.auto_exposure = _autoExposure || !a.exposure;
    nvmtl_rr_matrix(a.world_to_view, _worldToViewMatrix); nvmtl_rr_matrix(a.view_to_clip, _viewToClipMatrix);
    a.jitter_x = _jitterOffsetX; a.jitter_y = _jitterOffsetY; a.mv_scale_x = _motionVectorScaleX; a.mv_scale_y = _motionVectorScaleY;
    a.pre_exposure = _preExposure; a.reset = _shouldResetHistory;
    int rc;
    @synchronized (self) { rc = nvmtl_vk_cmd_dlss_rr(&cb->_c, &_feature, &a); }
    if (rc) { nvlog("dlss: RR: denoise was not recorded (output left as it was)"); return; }
    NVMTLTexture *all[] = { color, depth, motion, out, diff, spec, nrm, rgh };
    for (size_t k = 0; k < sizeof all / sizeof all[0]; k++) nvmtl_retain_resource(cb, all[k]);
    if (hit) nvmtl_retain_resource(cb, hit);
    if (expo && !_autoExposure) nvmtl_retain_resource(cb, expo);
    if (react) nvmtl_retain_resource(cb, react);
    NVMTLTexture *rs = out->_parent ? out->_parent : out;
    if (rs->_surf) nvmtl_surf_dirty(cb, rs);
}
@end

static BOOL nvmtl_mfx_dlss_rr_ready(void) { return nvmtl_vk_dlss_rr_available(); }
static id nvmtl_mfx_dlss_rr_make(id descriptor, id device) {
    if (!nvmtl_vk_dlss_rr_available()) return nil;
    MTLFXTemporalDenoisedScalerDescriptor *d = (MTLFXTemporalDenoisedScalerDescriptor *)descriptor;
    if (!d.inputWidth || !d.inputHeight || !d.outputWidth || !d.outputHeight || d.outputWidth < d.inputWidth || d.outputHeight < d.inputHeight
        || (double)d.outputWidth > 3.01 * (double)d.inputWidth || (double)d.outputHeight > 3.01 * (double)d.inputHeight) {
        nvlog("dlss: RR: denoised scaler %lux%lu -> %lux%lu is not a 1x-3x upscale - using the MetalFX path", (unsigned long)d.inputWidth,
              (unsigned long)d.inputHeight, (unsigned long)d.outputWidth, (unsigned long)d.outputHeight);
        return nil;
    }
    nvlog("dlss: MetalFX temporal denoised scaler %lux%lu -> %lux%lu served by DLSS Ray Reconstruction", (unsigned long)d.inputWidth,
          (unsigned long)d.inputHeight, (unsigned long)d.outputWidth, (unsigned long)d.outputHeight);
    return [[NVMTLDLSSDenoisedScaler alloc] initWithDescriptor:d];
}

// ---- Frame Generation behind MTLFXFrameInterpolator -------------------------------------------------------------------
// Handed out by the interpolator descriptor's newFrameInterpolatorWithDevice: hook (nvmtl_mfx_native_profile.inc) only for
// this driver's devices and only while the NGX core reports Frame Generation. The core keeps the previous real frame as
// its own history, so one MetalFX encode is one or two evaluations:
//   - when the app's prevColorTexture is not the colorTexture of the previous encode (or history is reset, or this is the
//     first encode) the previous frame is fed first as a priming evaluation (reset on the very first), then
//   - the current frame is evaluated and the core writes the in-between frame into outputTexture.
// Mapping: colorTexture -> DLSSG.Backbuffer, depthTexture -> DLSSG.Depth, motionTexture -> DLSSG.MVecs,
// outputTexture -> DLSSG.OutputInterpolated, uiTexture -> DLSSG.UI, motionVectorScale -> DLSSG.MvecScale, jitter -> DLSSG.Jitter.
// Camera: nearPlane/farPlane/fieldOfView(degrees -> radians)/aspectRatio give the projection. The interpolator carries no
// camera pose; when the descriptor's scaler offers worldToViewMatrix/viewToClipMatrix (the denoised scaler) the pose and
// ClipToPrevClip come from them (the previous frame's matrices are kept), otherwise the pose is the origin looking down -Z
// with an identity ClipToPrevClip and the motion vectors (which include camera motion) carry all of the movement.
// Not mapped (logged once): deltaTime, the fence, uiTextureComposited, and anything the core takes that MetalFX never gives
// (HUD-less colour, UI alpha, dynamic frame count).
// With a scaler attached, MetalFX takes the sizes and the motion vectors from the scaler. An app may then leave the
// descriptor's input/output sizes and motionTextureFormat unset (Cyberpunk 2077 does: it calls setScaler: and skips the
// size setters), and may set the interpolator's motionTexture to its depth texture. So: a size or the motion format the
// descriptor leaves at 0 is read from the scaler, and when the interpolator's motionTexture is missing or is the depth
// texture, the scaler's own motionTexture and motionVectorScale (the ones it was last encoded with) are used instead.
@interface NVMTLDLSSFrameInterpolator : NSObject <MTLFXFrameInterpolator> {
    void *_feature;
    id<MTLFXFrameInterpolatableScaler> _scaler;
    NSUInteger _inW, _inH, _outW, _outH;
    MTLPixelFormat _fmtColor, _fmtDepth, _fmtMotion, _fmtOutput, _fmtUI;
    id<MTLTexture> _lastColor;      // colorTexture of the previous encode, to tell whether prevColorTexture is the core's history
    BOOL _haveHistory, _notedUnmapped, _havePrevVP, _notedScalerMotion;
    simd_float4x4 _prevVP;
    uint32_t _frame;
}
- (instancetype)initWithDescriptor:(MTLFXFrameInterpolatorDescriptor *)d;
@end

@implementation NVMTLDLSSFrameInterpolator
@synthesize colorTexture = _colorTexture, prevColorTexture = _prevColorTexture, depthTexture = _depthTexture, motionTexture = _motionTexture;
@synthesize outputTexture = _outputTexture, uiTexture = _uiTexture, fence = _fence;
@synthesize motionVectorScaleX = _motionVectorScaleX, motionVectorScaleY = _motionVectorScaleY, deltaTime = _deltaTime;
@synthesize nearPlane = _nearPlane, farPlane = _farPlane, fieldOfView = _fieldOfView, aspectRatio = _aspectRatio;
@synthesize jitterOffsetX = _jitterOffsetX, jitterOffsetY = _jitterOffsetY, uiTextureComposited = _uiTextureComposited;
@synthesize shouldResetHistory = _shouldResetHistory, depthReversed = _depthReversed;

// A property of the descriptor's scaler, or 0 / nil when there is no scaler or it does not have that property.
static NSUInteger nvmtl_fg_scaler_uint(id s, SEL sel) { return s && [s respondsToSelector:sel] ? ((NSUInteger (*)(id, SEL))objc_msgSend)(s, sel) : 0; }
static float nvmtl_fg_scaler_float(id s, SEL sel, float dflt) { return s && [s respondsToSelector:sel] ? ((float (*)(id, SEL))objc_msgSend)(s, sel) : dflt; }
static id nvmtl_fg_scaler_obj(id s, SEL sel) { return s && [s respondsToSelector:sel] ? ((id (*)(id, SEL))objc_msgSend)(s, sel) : nil; }
// The descriptor's sizes, each one the descriptor leaves at 0 taken from its scaler. Returns YES when any came from the scaler.
static BOOL nvmtl_fg_sizes(MTLFXFrameInterpolatorDescriptor *d, NSUInteger s[4]) {
    id sc = d.scaler;
    NSUInteger own[4] = { d.inputWidth, d.inputHeight, d.outputWidth, d.outputHeight };
    SEL sel[4] = { @selector(inputWidth), @selector(inputHeight), @selector(outputWidth), @selector(outputHeight) };
    BOOL fromScaler = NO;
    for (int i = 0; i < 4; i++) {
        s[i] = own[i];
        if (!s[i]) { s[i] = nvmtl_fg_scaler_uint(sc, sel[i]); fromScaler |= s[i] != 0; }
    }
    return fromScaler;
}

- (instancetype)initWithDescriptor:(MTLFXFrameInterpolatorDescriptor *)d {
    if (!(self = [super init])) return nil;
    NSUInteger s[4]; nvmtl_fg_sizes(d, s);
    _inW = s[0]; _inH = s[1]; _outW = s[2]; _outH = s[3];
    _fmtColor = d.colorTextureFormat; _fmtDepth = d.depthTextureFormat; _fmtMotion = d.motionTextureFormat;
    _fmtOutput = d.outputTextureFormat; _fmtUI = d.uiTextureFormat;
    _scaler = d.scaler;
    if (_fmtMotion == MTLPixelFormatInvalid) _fmtMotion = (MTLPixelFormat)nvmtl_fg_scaler_uint(_scaler, @selector(motionTextureFormat));
    _motionVectorScaleX = 1.0f; _motionVectorScaleY = 1.0f; _depthReversed = YES;   // the protocol's documented default
    _nearPlane = 0.1f; _farPlane = 1000.0f; _fieldOfView = 60.0f; _aspectRatio = _outH ? (float)_outW / (float)_outH : 1.0f;
    return self;
}
- (MTLTextureUsage)colorTextureUsage { return MTLTextureUsageShaderRead; }
- (MTLTextureUsage)depthTextureUsage { return MTLTextureUsageShaderRead; }
- (MTLTextureUsage)motionTextureUsage { return MTLTextureUsageShaderRead; }
- (MTLTextureUsage)uiTextureUsage { return MTLTextureUsageShaderRead; }
- (MTLTextureUsage)outputTextureUsage { return MTLTextureUsageShaderRead | MTLTextureUsageShaderWrite; }
- (MTLPixelFormat)colorTextureFormat { return _fmtColor; }
- (MTLPixelFormat)depthTextureFormat { return _fmtDepth; }
- (MTLPixelFormat)motionTextureFormat { return _fmtMotion; }
- (MTLPixelFormat)outputTextureFormat { return _fmtOutput; }
- (MTLPixelFormat)uiTextureFormat { return _fmtUI; }
- (NSUInteger)inputWidth { return _inW; }
- (NSUInteger)inputHeight { return _inH; }
- (NSUInteger)outputWidth { return _outW; }
- (NSUInteger)outputHeight { return _outH; }

static void nvmtl_fg_copy(float out[16], simd_float4x4 m) { for (int c = 0; c < 4; c++) for (int r = 0; r < 4; r++) out[c * 4 + r] = m.columns[c][r]; }

// One call into the Vulkan layer for `color` with the shared arguments in `base`; the matrices and pose are set by the caller.
- (int)evaluate:(NVMTLCommandBuffer *)cb args:(nvmtl_vk_dlss_fg_args)a color:(NVMTLTexture *)color reset:(BOOL)reset {
    a.color = [color nvi]; a.color_view = [color nvview]; a.reset = reset;
    int rc;
    @synchronized (self) { rc = nvmtl_vk_cmd_dlss_fg(&cb->_c, &_feature, &a); }
    return rc;
}

- (void)encodeToCommandBuffer:(id<MTLCommandBuffer>)commandBuffer {
    NVMTLCommandBuffer *cb = nvmtl_dlss_recording_cb(commandBuffer);
    if (!cb) { nvlog("dlss: FG: the command buffer is not this driver's - interpolation skipped"); return; }
    NVMTLTexture *color = nvmtl_dlss_tex(_colorTexture), *prev = nvmtl_dlss_tex(_prevColorTexture), *depth = nvmtl_dlss_tex(_depthTexture);
    NVMTLTexture *motion = nvmtl_dlss_tex(_motionTexture), *out = nvmtl_dlss_tex(_outputTexture), *ui = nvmtl_dlss_tex(_uiTexture);
    float mvScaleX = _motionVectorScaleX, mvScaleY = _motionVectorScaleY;
    if (_scaler && (!_motionTexture || (id)_motionTexture == (id)_depthTexture)) {
        // Depth handed in as motion (or no motion at all): the scaler holds the real motion vectors of this frame.
        id<MTLTexture> sm = nvmtl_fg_scaler_obj(_scaler, @selector(motionTexture));
        NVMTLTexture *smt = nvmtl_dlss_tex(sm);
        if (smt) {
            motion = smt;
            mvScaleX = nvmtl_fg_scaler_float(_scaler, @selector(motionVectorScaleX), mvScaleX);
            mvScaleY = nvmtl_fg_scaler_float(_scaler, @selector(motionVectorScaleY), mvScaleY);
            if (!_notedScalerMotion) {
                _notedScalerMotion = YES;
                nvlog("dlss: FG: motion vectors from the temporal scaler (%lux%lu, format %lu, scale %.1f x %.1f) instead of the interpolator's %s",
                      (unsigned long)sm.width, (unsigned long)sm.height, (unsigned long)sm.pixelFormat, mvScaleX, mvScaleY,
                      _motionTexture ? "depth texture" : "empty motion texture");
            }
        }
    }
    if (!color || !depth || !motion || !out) { nvlog("dlss: FG: colour, depth, motion and output textures must all be set (and be this driver's) - interpolation skipped"); return; }
    if (_prevColorTexture && !prev) { nvlog("dlss: FG: the previous colour texture is not this driver's - interpolation skipped"); return; }
    if (_uiTexture && !ui) { nvlog("dlss: FG: the UI texture is not this driver's - interpolation skipped"); return; }
    if (!_notedUnmapped) {
        _notedUnmapped = YES;
        if (_fence || _deltaTime > 0 || _uiTextureComposited) nvlog("dlss: FG: deltaTime, the fence and uiTextureComposited are not mapped to NGX (ignored)");
    }
    nvmtl_vk_dlss_fg_args a = {0};
    a.depth = [depth nvi]; a.depth_view = [depth nvview]; a.motion = [motion nvi]; a.motion_view = [motion nvview];
    a.output = [out nvi]; a.output_view = [out nvview];
    if (ui) { a.ui = [ui nvi]; a.ui_view = [ui nvview]; }
    a.in_w = (uint32_t)_inW; a.in_h = (uint32_t)_inH; a.out_w = (uint32_t)_outW; a.out_h = (uint32_t)_outH;
    a.hdr = nvmtl_dlss_float_format(_fmtColor); a.depth_inverted = _depthReversed; a.camera_motion_included = 1;
    a.near_z = _nearPlane; a.far_z = _farPlane; a.fov_y = _fieldOfView * (float)M_PI / 180.0f; a.aspect = _aspectRatio;
    a.mv_scale_x = mvScaleX; a.mv_scale_y = mvScaleY; a.jitter_x = _jitterOffsetX; a.jitter_y = _jitterOffsetY;
    a.multi_index = 1; a.multi_count = 1; a.frame_id = ++_frame;
    // Camera pose and the clip-space step from the previous frame, when the scaler has the matrices.
    simd_float4x4 id4 = matrix_identity_float4x4, v = id4, p = id4, vp = id4;
    BOOL haveMat = NO;
    id sc = _scaler;
    if (sc && [sc respondsToSelector:@selector(worldToViewMatrix)] && [sc respondsToSelector:@selector(viewToClipMatrix)]) {
        v = ((simd_float4x4 (*)(id, SEL))objc_msgSend)(sc, @selector(worldToViewMatrix));
        p = ((simd_float4x4 (*)(id, SEL))objc_msgSend)(sc, @selector(viewToClipMatrix));
        haveMat = YES;
    }
    vp = simd_mul(p, v);
    simd_float4x4 toPrev = id4, fromPrev = id4;
    if (haveMat && _havePrevVP && !_shouldResetHistory) { toPrev = simd_mul(_prevVP, simd_inverse(vp)); fromPrev = simd_inverse(toPrev); }
    // View rows are the camera axes in world space; the camera looks down -Z of view space, and sits at -R^T t.
    simd_float3 right = { v.columns[0][0], v.columns[1][0], v.columns[2][0] }, up = { v.columns[0][1], v.columns[1][1], v.columns[2][1] };
    simd_float3 fwd = { -v.columns[0][2], -v.columns[1][2], -v.columns[2][2] }, t = { v.columns[3][0], v.columns[3][1], v.columns[3][2] };
    simd_float3 pos = -(right * t.x + up * t.y - fwd * t.z);
    a.cam_right[0] = right.x; a.cam_right[1] = right.y; a.cam_right[2] = right.z;
    a.cam_up[0] = up.x; a.cam_up[1] = up.y; a.cam_up[2] = up.z;
    a.cam_fwd[0] = fwd.x; a.cam_fwd[1] = fwd.y; a.cam_fwd[2] = fwd.z;
    a.cam_pos[0] = pos.x; a.cam_pos[1] = pos.y; a.cam_pos[2] = pos.z;
    nvmtl_fg_copy(a.clip_to_prev, toPrev); nvmtl_fg_copy(a.prev_to_clip, fromPrev);
    nvmtl_fg_copy(a.view_to_clip, haveMat ? p : id4);

    int rc = 0;
    // Prime the core's history with the previous real frame when it has not already seen it as the last encode's colour.
    BOOL reset = _shouldResetHistory;   // a scene cut: the current frame alone starts the history
    if (!reset && prev && (!_haveHistory || (id)_prevColorTexture != (id)_lastColor)) {
        rc = [self evaluate:cb args:a color:prev reset:!_haveHistory];
        if (!rc) nvmtl_retain_resource(cb, prev);
    } else if (!reset && !_haveHistory) reset = YES;   // no previous frame at all: the first frame starts the history
    if (!rc) rc = [self evaluate:cb args:a color:color reset:reset];
    if (rc) { nvlog("dlss: FG: interpolation was not recorded (output left as it was)"); _haveHistory = NO; return; }
    _haveHistory = YES; _lastColor = _colorTexture; if (haveMat) { _prevVP = vp; _havePrevVP = YES; }
    nvmtl_retain_resource(cb, color); nvmtl_retain_resource(cb, depth); nvmtl_retain_resource(cb, motion); nvmtl_retain_resource(cb, out);
    if (ui) nvmtl_retain_resource(cb, ui);
    NVMTLTexture *rs = out->_parent ? out->_parent : out;
    if (rs->_surf) nvmtl_surf_dirty(cb, rs);
}
@end

static BOOL nvmtl_mfx_dlss_fg_ready(void) { return nvmtl_vk_dlss_fg_available(); }
static id nvmtl_mfx_dlss_fg_make(id descriptor, id device) {
    if (!nvmtl_vk_dlss_fg_available()) return nil;
    MTLFXFrameInterpolatorDescriptor *d = (MTLFXFrameInterpolatorDescriptor *)descriptor;
    NSUInteger s[4];
    BOOL fromScaler = nvmtl_fg_sizes(d, s);
    if (!s[0] || !s[1] || !s[2] || !s[3]) {
        nvlog("dlss: FG: interpolator has no sizes (scaler %s) - using the MetalFX path", d.scaler ? "attached but without sizes" : "none");
        return nil;
    }
    nvlog("dlss: MetalFX frame interpolator %lux%lu (depth/motion %lux%lu%s) served by DLSS Frame Generation", (unsigned long)s[2],
          (unsigned long)s[3], (unsigned long)s[0], (unsigned long)s[1], fromScaler ? ", sizes from the temporal scaler" : "");
    return [[NVMTLDLSSFrameInterpolator alloc] initWithDescriptor:d];
}
#pragma clang diagnostic pop
