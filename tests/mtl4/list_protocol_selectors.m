// Lists every selector of the Metal protocols the plugin implements, as the OS's own Metal headers declare them, and
// optionally which of them a class does not answer.
//
//   list_protocol_selectors                        all selectors of the protocols below, one per line
//   list_protocol_selectors --class NVMTLDevice    only the ones instances of that class do NOT respond to
//   list_protocol_selectors --load <dylib> --class NVMTLDevice
//                                                  dlopen the plugin first so its classes are registered
//   list_protocol_selectors --protocol MTLDevice --class NVMTLDevice
//                                                  restrict to one protocol (and the ones it incorporates)
//
// Line format: required|optional  instance|class  protocol  selector
// The protocol list is compiled against the SDK it is built with, so build it with the newest SDK.
#import <Foundation/Foundation.h>
#import <Metal/Metal.h>
#import <objc/runtime.h>
#include <dlfcn.h>

static void walk(Protocol *p, NSMutableSet *seen, Class cls)
{
    NSString *pn = NSStringFromProtocol(p);
    if ([seen containsObject:pn] || [pn isEqualToString:@"NSObject"]) return;
    [seen addObject:pn];
    for (int req = 1; req >= 0; req--) for (int inst = 1; inst >= 0; inst--) {
        unsigned n = 0; struct objc_method_description *m = protocol_copyMethodDescriptionList(p, req, inst, &n);
        for (unsigned i = 0; i < n; i++) {
            const BOOL responds = !cls ? NO : inst ? [cls instancesRespondToSelector:m[i].name] : [cls respondsToSelector:m[i].name];
            if (cls && responds) continue;
            printf("%s %s %s %s\n", req ? "required" : "optional", inst ? "instance" : "class", pn.UTF8String, sel_getName(m[i].name));
        }
        free(m);
    }
    unsigned n = 0; Protocol * __unsafe_unretained *inc = protocol_copyProtocolList(p, &n);
    for (unsigned i = 0; i < n; i++) walk(inc[i], seen, cls);
    free(inc);
}

int main(int argc, char **argv)
{
    @autoreleasepool {
        const char *clsName = NULL, *protoName = NULL;
        for (int i = 1; i < argc; i++) {
            if (!strcmp(argv[i], "--class") && i + 1 < argc) clsName = argv[++i];
            else if (!strcmp(argv[i], "--protocol") && i + 1 < argc) protoName = argv[++i];
            else if (!strcmp(argv[i], "--load") && i + 1 < argc) {
                if (!dlopen(argv[++i], RTLD_NOW)) { fprintf(stderr, "cannot load %s: %s\n", argv[i], dlerror()); return 1; }
            }
        }
        if (clsName && !objc_getClass(clsName)) (void)MTLCreateSystemDefaultDevice();   // loads the GPU driver bundles, which register their classes
        Class cls = clsName ? objc_getClass(clsName) : Nil;
        if (clsName && !cls) { fprintf(stderr, "no class %s\n", clsName); return 1; }
        NSMutableSet *seen = [NSMutableSet set];
        NSArray<Protocol *> *roots = @[
            @protocol(MTLDevice), @protocol(MTLCommandQueue), @protocol(MTLCommandBuffer), @protocol(MTLBuffer), @protocol(MTLTexture),
            @protocol(MTLHeap), @protocol(MTLResidencySet), @protocol(MTLSharedEvent), @protocol(MTLEvent), @protocol(MTLFence),
            @protocol(MTLTensor), @protocol(MTLTextureViewPool), @protocol(MTL4CounterHeap), @protocol(MTLBlitCommandEncoder),
            @protocol(MTLComputeCommandEncoder), @protocol(MTLRenderCommandEncoder), @protocol(MTLArgumentEncoder), @protocol(MTLLibrary),
            @protocol(MTLComputePipelineState), @protocol(MTLRenderPipelineState), @protocol(MTLDepthStencilState), @protocol(MTLSamplerState) ];
        if (protoName) {
            Protocol *only = NSProtocolFromString([NSString stringWithUTF8String:protoName]);
            if (!only) { fprintf(stderr, "no protocol %s\n", protoName); return 1; }
            roots = @[only];
        }
        for (Protocol *p in roots) walk(p, seen, cls);
    }
    return 0;
}
