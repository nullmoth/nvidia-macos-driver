static __thread unsigned nvmtl_mfx_profile_depth;
static BOOL (*nvmtl_mfx_support_original)(id, SEL, id);
static float (*nvmtl_mfx_min_original)(id, SEL, id);
static float (*nvmtl_mfx_max_original)(id, SEL, id);
typedef id (*nvmtl_mfx_factory_fn)(id, SEL, id) __attribute__((ns_returns_retained));
static nvmtl_mfx_factory_fn nvmtl_mfx_new_original;
// DLSS behind the temporal scaler (NVMTLMetalFXDLSS.m, defined further down this translation unit).
static BOOL nvmtl_mfx_dlss_ready(void);
static id nvmtl_mfx_dlss_make(id descriptor, id device);
static BOOL nvmtl_mfx_dlss_rr_ready(void);
static id nvmtl_mfx_dlss_rr_make(id descriptor, id device);
static BOOL nvmtl_mfx_dlss_fg_ready(void);
static id nvmtl_mfx_dlss_fg_make(id descriptor, id device);
static BOOL (*nvmtl_mfx_fg_support_original)(id, SEL, id);
static nvmtl_mfx_factory_fn nvmtl_mfx_fg_new_original;
// The denoised scaler's descriptor: only the factory and the support/scale answers are hooked, and only for DLSS.
static BOOL (*nvmtl_mfx_rr_support_original)(id, SEL, id);
static float (*nvmtl_mfx_rr_min_original)(id, SEL, id);
static float (*nvmtl_mfx_rr_max_original)(id, SEL, id);
static nvmtl_mfx_factory_fn nvmtl_mfx_rr_new_original;
// The native profile's depth counter and answers apply only when that profile is on; with only DLSS installed the
// descriptor hooks leave every non-DLSS answer to the original implementation.
static BOOL nvmtl_mfx_native_on;
static BOOL nvmtl_mfx_dlss_on;
static BOOL nvmtl_mfx_our_device(id device) {
    return device && [NSStringFromClass(object_getClass(device)) isEqualToString:@"NVMTLDevice"];
}
static BOOL nvmtl_mfx_support(id receiver, SEL selector, id device) {
    if (!nvmtl_mfx_our_device(device)) return nvmtl_mfx_support_original(receiver, selector, device);
    if (nvmtl_mfx_dlss_on && nvmtl_mfx_dlss_ready()) return YES;
    if (!nvmtl_mfx_native_on) return nvmtl_mfx_support_original(receiver, selector, device);
    ++nvmtl_mfx_profile_depth;
    @try { return nvmtl_mfx_support_original(receiver, selector, device); }
    @finally { --nvmtl_mfx_profile_depth; }
}
static float nvmtl_mfx_min(id receiver, SEL selector, id device) {
    if (!nvmtl_mfx_our_device(device)) return nvmtl_mfx_min_original(receiver, selector, device);
    if (nvmtl_mfx_dlss_on && nvmtl_mfx_dlss_ready()) {
        // Report what the native profile reports, narrowed to the range DLSS takes, so an app's quality menu is unchanged.
        float v = NVMTL_DLSS_MIN_INPUT_SCALE;
        if (nvmtl_mfx_native_on) { ++nvmtl_mfx_profile_depth; v = nvmtl_mfx_min_original(receiver, selector, device); --nvmtl_mfx_profile_depth;
                                   if (!(v >= NVMTL_DLSS_MIN_INPUT_SCALE)) v = NVMTL_DLSS_MIN_INPUT_SCALE; }
        return v;
    }
    if (!nvmtl_mfx_native_on) return nvmtl_mfx_min_original(receiver, selector, device);
    ++nvmtl_mfx_profile_depth;
    @try { return nvmtl_mfx_min_original(receiver, selector, device); }
    @finally { --nvmtl_mfx_profile_depth; }
}
static float nvmtl_mfx_max(id receiver, SEL selector, id device) {
    if (!nvmtl_mfx_our_device(device)) return nvmtl_mfx_max_original(receiver, selector, device);
    if (nvmtl_mfx_dlss_on && nvmtl_mfx_dlss_ready()) {
        float v = NVMTL_DLSS_MAX_INPUT_SCALE;
        if (nvmtl_mfx_native_on) { ++nvmtl_mfx_profile_depth; v = nvmtl_mfx_max_original(receiver, selector, device); --nvmtl_mfx_profile_depth;
                                   if (!(v > 0) || v > NVMTL_DLSS_MAX_INPUT_SCALE) v = NVMTL_DLSS_MAX_INPUT_SCALE; }
        return v;
    }
    if (!nvmtl_mfx_native_on) return nvmtl_mfx_max_original(receiver, selector, device);
    ++nvmtl_mfx_profile_depth;
    @try { return nvmtl_mfx_max_original(receiver, selector, device); }
    @finally { --nvmtl_mfx_profile_depth; }
}
static id nvmtl_mfx_new(id receiver, SEL selector, id device) __attribute__((ns_returns_retained));
static id nvmtl_mfx_new(id receiver, SEL selector, id device) {
    if (!nvmtl_mfx_our_device(device)) return nvmtl_mfx_new_original(receiver, selector, device);
    if (nvmtl_mfx_dlss_on) {
        // DLSS serves the scaler when it is ready and the descriptor is one it can take; otherwise fall through to the path below.
        id dlss = nvmtl_mfx_dlss_make(receiver, device);
        if (dlss) return dlss;
    }
    if (!nvmtl_mfx_native_on) return nvmtl_mfx_new_original(receiver, selector, device);
    ++nvmtl_mfx_profile_depth;
    @try {
        id scaler = nvmtl_mfx_new_original(receiver, selector, device);
        nvlog("MetalFX temporal native profile: %s", scaler ? NSStringFromClass(object_getClass(scaler)).UTF8String : "unavailable");
        return scaler;
    } @finally { --nvmtl_mfx_profile_depth; }
}
// Every MetalFX support answer is logged (first 4 per question): an application hides an upscaler, denoiser or frame
// generation option when these say NO, and the option list in its settings is the only other trace of the answer.
static BOOL nvmtl_mfx_note(const char *what, id device, BOOL ours, BOOL yes) {
    static _Atomic uint32_t n[8]; uint32_t slot = (uint32_t)(((uintptr_t)what >> 3) & 7);
    if (atomic_fetch_add(&n[slot], 1) < 4) nvlog("QUERY MetalFX %s -> %s (device %s)", what, yes ? "YES" : "NO", ours ? "ours" : "not ours");
    return yes;
}
static BOOL nvmtl_mfx_rr_support(id receiver, SEL selector, id device) {
    const BOOL ours = nvmtl_mfx_our_device(device);
    if (ours && nvmtl_mfx_dlss_rr_ready()) return nvmtl_mfx_note("denoised scaler supportsDevice:", device, ours, YES);
    return nvmtl_mfx_note("denoised scaler supportsDevice:", device, ours, nvmtl_mfx_rr_support_original(receiver, selector, device));
}
static float nvmtl_mfx_rr_min(id receiver, SEL selector, id device) {
    if (nvmtl_mfx_our_device(device) && nvmtl_mfx_dlss_rr_ready()) return NVMTL_DLSS_MIN_INPUT_SCALE;
    return nvmtl_mfx_rr_min_original(receiver, selector, device);
}
static float nvmtl_mfx_rr_max(id receiver, SEL selector, id device) {
    if (nvmtl_mfx_our_device(device) && nvmtl_mfx_dlss_rr_ready()) return NVMTL_DLSS_MAX_INPUT_SCALE;
    return nvmtl_mfx_rr_max_original(receiver, selector, device);
}
static id nvmtl_mfx_rr_new(id receiver, SEL selector, id device) __attribute__((ns_returns_retained));
static id nvmtl_mfx_rr_new(id receiver, SEL selector, id device) {
    if (nvmtl_mfx_our_device(device)) {
        id rr = nvmtl_mfx_dlss_rr_make(receiver, device);
        if (rr) return rr;
    }
    return nvmtl_mfx_rr_new_original(receiver, selector, device);
}
static BOOL nvmtl_mfx_fg_support(id receiver, SEL selector, id device) {
    const BOOL ours = nvmtl_mfx_our_device(device);
    if (ours && nvmtl_mfx_dlss_fg_ready()) return nvmtl_mfx_note("frame interpolator supportsDevice:", device, ours, YES);
    return nvmtl_mfx_note("frame interpolator supportsDevice:", device, ours, nvmtl_mfx_fg_support_original(receiver, selector, device));
}
static id nvmtl_mfx_fg_new(id receiver, SEL selector, id device) __attribute__((ns_returns_retained));
static id nvmtl_mfx_fg_new(id receiver, SEL selector, id device) {
    if (nvmtl_mfx_our_device(device)) {
        id fg = nvmtl_mfx_dlss_fg_make(receiver, device);
        if (fg) return fg;
    }
    return nvmtl_mfx_fg_new_original(receiver, selector, device);
}
static void nvmtl_mfx_fg_install(void) {
    Class type = NSClassFromString(@"MTLFXFrameInterpolatorDescriptor");
    if (!type) return;
    Method support = class_getClassMethod(type, @selector(supportsDevice:));
    Method factory = class_getInstanceMethod(type, @selector(newFrameInterpolatorWithDevice:));
    if (!support || !factory) { nvlog("MetalFX frame interpolator: API unavailable - Frame Generation hooks not installed"); return; }
    nvmtl_mfx_fg_support_original = (void *)method_getImplementation(support);
    nvmtl_mfx_fg_new_original = (nvmtl_mfx_factory_fn)method_getImplementation(factory);
    method_setImplementation(support, (IMP)nvmtl_mfx_fg_support);
    method_setImplementation(factory, (IMP)nvmtl_mfx_fg_new);
    nvlog("MetalFX frame interpolator: Frame Generation hooks installed for NVMTL devices");
}
// Metal 4 FX (macOS 26): the same DLSS objects stand behind +supportsMetal4FX: and the new...WithDevice:compiler:
// factories of the temporal scaler, the denoised scaler and the frame interpolator. The compiler argument is not needed:
// DLSS brings its own kernels. The objects take an MTL4 command buffer in encodeToCommandBuffer: through
// nvmtl_dlss_recording_cb() (NVMTLMetalFXDLSS.m). NVMTL_METAL4=0 turns the Metal 4 answers off; everything else stays.
typedef BOOL (*nvmtl_mfx_bool_fn)(id, SEL, id);
typedef id (*nvmtl_mfx_factory2_fn)(id, SEL, id, id) __attribute__((ns_returns_retained));
static nvmtl_mfx_bool_fn nvmtl_m4fx_support_o[3];
static nvmtl_mfx_factory2_fn nvmtl_m4fx_new_o[3];
static BOOL nvmtl_mtl4_wanted(void) { const char *e = getenv("NVMTL_METAL4"); return !(e && e[0] == '0'); }
static BOOL nvmtl_m4fx_ready(int kind) {
    return kind == 0 ? nvmtl_mfx_dlss_ready() : kind == 1 ? nvmtl_mfx_dlss_rr_ready() : nvmtl_mfx_dlss_fg_ready();
}
static id nvmtl_m4fx_make(int kind, id descriptor, id device) {
    return kind == 0 ? nvmtl_mfx_dlss_make(descriptor, device) : kind == 1 ? nvmtl_mfx_dlss_rr_make(descriptor, device)
                                                                          : nvmtl_mfx_dlss_fg_make(descriptor, device);
}
#define NVMTL_M4FX_PAIR(k)                                                                                               \
    static BOOL nvmtl_m4fx_support_##k(id r, SEL s, id d) {                                                              \
        static const char *const q = k == 0 ? "temporal scaler supportsMetal4FX:" : k == 1 ? "denoised scaler supportsMetal4FX:" \
                                            : "frame interpolator supportsMetal4FX:";                                     \
        if (nvmtl_mtl4_wanted() && nvmtl_mfx_our_device(d) && nvmtl_m4fx_ready(k)) return nvmtl_mfx_note(q, d, YES, YES); \
        return nvmtl_mfx_note(q, d, nvmtl_mfx_our_device(d), nvmtl_m4fx_support_o[k] ? nvmtl_m4fx_support_o[k](r, s, d) : NO); \
    }                                                                                                                     \
    static id nvmtl_m4fx_new_##k(id r, SEL s, id d, id c) __attribute__((ns_returns_retained));                          \
    static id nvmtl_m4fx_new_##k(id r, SEL s, id d, id c) {                                                              \
        if (nvmtl_mtl4_wanted() && nvmtl_mfx_our_device(d)) { id x = nvmtl_m4fx_make(k, r, d); if (x) return x; }        \
        return nvmtl_m4fx_new_o[k] ? nvmtl_m4fx_new_o[k](r, s, d, c) : nil;                                              \
    }
NVMTL_M4FX_PAIR(0)
NVMTL_M4FX_PAIR(1)
NVMTL_M4FX_PAIR(2)
#undef NVMTL_M4FX_PAIR
static void nvmtl_mfx_mtl4fx_install(void) {
    static const char *const desc[3] = { "MTLFXTemporalScalerDescriptor", "MTLFXTemporalDenoisedScalerDescriptor",
                                         "MTLFXFrameInterpolatorDescriptor" };
    static const char *const factory[3] = { "newTemporalScalerWithDevice:compiler:", "newTemporalDenoisedScalerWithDevice:compiler:",
                                            "newFrameInterpolatorWithDevice:compiler:" };
    static const char *const proto[3] = { "MTL4FXTemporalScaler", "MTL4FXTemporalDenoisedScaler", "MTL4FXFrameInterpolator" };
    static const char *const impl[3] = { "NVMTLDLSSScaler", "NVMTLDLSSDenoisedScaler", "NVMTLDLSSFrameInterpolator" };
    const IMP sup[3] = { (IMP)nvmtl_m4fx_support_0, (IMP)nvmtl_m4fx_support_1, (IMP)nvmtl_m4fx_support_2 };
    const IMP mk[3] = { (IMP)nvmtl_m4fx_new_0, (IMP)nvmtl_m4fx_new_1, (IMP)nvmtl_m4fx_new_2 };
    for (int k = 0; k < 3; k++) {
        Class type = objc_getClass(desc[k]);
        Method support = type ? class_getClassMethod(type, sel_registerName("supportsMetal4FX:")) : NULL;
        Method make = type ? class_getInstanceMethod(type, sel_registerName(factory[k])) : NULL;
        if (!support || !make) { nvlog("MetalFX %s: no Metal 4 FX API - not hooked", desc[k]); continue; }
        nvmtl_m4fx_support_o[k] = (nvmtl_mfx_bool_fn)method_getImplementation(support);
        nvmtl_m4fx_new_o[k] = (nvmtl_mfx_factory2_fn)method_getImplementation(make);
        method_setImplementation(support, sup[k]);
        method_setImplementation(make, mk[k]);
        // The objects already answer encodeToCommandBuffer:; declaring the Metal 4 protocol lets conformsToProtocol: say so.
        Protocol *p = objc_getProtocol(proto[k]);
        Class c = objc_getClass(impl[k]);
        if (p && c) class_addProtocol(c, p);
        nvlog("MetalFX %s: Metal 4 FX hooks installed (DLSS)", desc[k]);
    }
}
// Called from the constructor below when DLSS is on. Quietly does nothing where the denoised scaler does not exist.
static void nvmtl_mfx_rr_install(void) {
    Class type = NSClassFromString(@"MTLFXTemporalDenoisedScalerDescriptor");
    if (!type) return;
    Method support = class_getClassMethod(type, @selector(supportsDevice:));
    Method min = class_getClassMethod(type, @selector(supportedInputContentMinScaleForDevice:));
    Method max = class_getClassMethod(type, @selector(supportedInputContentMaxScaleForDevice:));
    Method factory = class_getInstanceMethod(type, @selector(newTemporalDenoisedScalerWithDevice:));
    if (!support || !min || !max || !factory) { nvlog("MetalFX denoised scaler: API unavailable - Ray Reconstruction hooks not installed"); return; }
    nvmtl_mfx_rr_support_original = (void *)method_getImplementation(support);
    nvmtl_mfx_rr_min_original = (void *)method_getImplementation(min);
    nvmtl_mfx_rr_max_original = (void *)method_getImplementation(max);
    nvmtl_mfx_rr_new_original = (nvmtl_mfx_factory_fn)method_getImplementation(factory);
    method_setImplementation(support, (IMP)nvmtl_mfx_rr_support);
    method_setImplementation(min, (IMP)nvmtl_mfx_rr_min);
    method_setImplementation(max, (IMP)nvmtl_mfx_rr_max);
    method_setImplementation(factory, (IMP)nvmtl_mfx_rr_new);
    nvlog("MetalFX temporal denoised scaler: Ray Reconstruction hooks installed for NVMTL devices");
}
__attribute__((constructor)) static void nvmtl_mfx_profile_install(void) {
    // This constructor runs before the one that reads nvrm610.conf; settle the knobs first so a conf or profile value counts.
    nvmtl_conf_settle();
    const char *enabled = getenv("NVMTL_METALFX_NATIVE_PROFILE");
    nvmtl_mfx_native_on = enabled && !strcmp(enabled, "1");
    // DLSS is on when its files are installed and NVMTL_DLSS is not 0 (nvmtl_dlss_wanted); without them this constructor
    // does exactly what it did before.
    nvmtl_mfx_dlss_on = nvmtl_dlss_wanted() != 0;
    if (!nvmtl_mfx_native_on && !nvmtl_mfx_dlss_on) return;
    if (nvmtl_mfx_dlss_on) { nvmtl_mfx_rr_install(); nvmtl_mfx_fg_install(); nvmtl_mfx_mtl4fx_install(); }
    Class type = Nil;
    if (@available(macOS 13.0, *)) type = [MTLFXTemporalScalerDescriptor class];   // no MetalFX before macOS 13
    Method support = class_getClassMethod(type, @selector(supportsDevice:));
    Method min = class_getClassMethod(type, @selector(supportedInputContentMinScaleForDevice:));
    Method max = class_getClassMethod(type, @selector(supportedInputContentMaxScaleForDevice:));
    Method factory = class_getInstanceMethod(type, @selector(newTemporalScalerWithDevice:));
    if (!support || !min || !max || !factory) { nvlog("MetalFX temporal native profile: API unavailable"); return; }
    nvmtl_mfx_support_original = (void *)method_getImplementation(support);
    nvmtl_mfx_min_original = (void *)method_getImplementation(min);
    nvmtl_mfx_max_original = (void *)method_getImplementation(max);
    nvmtl_mfx_new_original = (nvmtl_mfx_factory_fn)method_getImplementation(factory);
    method_setImplementation(support, (IMP)nvmtl_mfx_support);
    method_setImplementation(min, (IMP)nvmtl_mfx_min);
    method_setImplementation(max, (IMP)nvmtl_mfx_max);
    method_setImplementation(factory, (IMP)nvmtl_mfx_new);
    if (nvmtl_mfx_native_on) nvlog("MetalFX temporal native profile: installed for NVMTL devices");
    if (nvmtl_mfx_dlss_on) nvlog("MetalFX temporal scaler: DLSS hooks installed for NVMTL devices");
}
