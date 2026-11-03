static __thread unsigned nvmtl_mfx_profile_depth;
static BOOL (*nvmtl_mfx_support_original)(id, SEL, id);
static float (*nvmtl_mfx_min_original)(id, SEL, id);
static float (*nvmtl_mfx_max_original)(id, SEL, id);
typedef id (*nvmtl_mfx_factory_fn)(id, SEL, id) __attribute__((ns_returns_retained));
static nvmtl_mfx_factory_fn nvmtl_mfx_new_original;
static BOOL nvmtl_mfx_our_device(id device) {
    return device && [NSStringFromClass(object_getClass(device)) isEqualToString:@"NVMTLDevice"];
}
static BOOL nvmtl_mfx_support(id receiver, SEL selector, id device) {
    if (!nvmtl_mfx_our_device(device)) return nvmtl_mfx_support_original(receiver, selector, device);
    ++nvmtl_mfx_profile_depth;
    @try { return nvmtl_mfx_support_original(receiver, selector, device); }
    @finally { --nvmtl_mfx_profile_depth; }
}
static float nvmtl_mfx_min(id receiver, SEL selector, id device) {
    if (!nvmtl_mfx_our_device(device)) return nvmtl_mfx_min_original(receiver, selector, device);
    ++nvmtl_mfx_profile_depth;
    @try { return nvmtl_mfx_min_original(receiver, selector, device); }
    @finally { --nvmtl_mfx_profile_depth; }
}
static float nvmtl_mfx_max(id receiver, SEL selector, id device) {
    if (!nvmtl_mfx_our_device(device)) return nvmtl_mfx_max_original(receiver, selector, device);
    ++nvmtl_mfx_profile_depth;
    @try { return nvmtl_mfx_max_original(receiver, selector, device); }
    @finally { --nvmtl_mfx_profile_depth; }
}
static id nvmtl_mfx_new(id receiver, SEL selector, id device) __attribute__((ns_returns_retained));
static id nvmtl_mfx_new(id receiver, SEL selector, id device) {
    if (!nvmtl_mfx_our_device(device)) return nvmtl_mfx_new_original(receiver, selector, device);
    ++nvmtl_mfx_profile_depth;
    @try {
        id scaler = nvmtl_mfx_new_original(receiver, selector, device);
        nvlog("MetalFX temporal native profile: %s", scaler ? NSStringFromClass(object_getClass(scaler)).UTF8String : "unavailable");
        return scaler;
    } @finally { --nvmtl_mfx_profile_depth; }
}
__attribute__((constructor)) static void nvmtl_mfx_profile_install(void) {
    const char *enabled = getenv("NVMTL_METALFX_NATIVE_PROFILE");
    if (!enabled || strcmp(enabled, "1")) return;
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
    nvlog("MetalFX temporal native profile: installed for NVMTL devices");
}
