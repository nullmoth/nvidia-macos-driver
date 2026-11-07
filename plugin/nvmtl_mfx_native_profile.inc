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
static BOOL nvmtl_mfx_rr_support(id receiver, SEL selector, id device) {
    if (nvmtl_mfx_our_device(device) && nvmtl_mfx_dlss_rr_ready()) return YES;
    return nvmtl_mfx_rr_support_original(receiver, selector, device);
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
    if (nvmtl_mfx_dlss_on) nvmtl_mfx_rr_install();
    Class type = MTLFXTemporalScalerDescriptor.class;
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
