#import <Metal/Metal.h>
#include <stdio.h>
#include <objc/runtime.h>
static void dumpProps(id o) {
    unsigned n = 0; Class c = [o class];
    printf("    class %s\n", class_getName(c));
    Protocol * __unsafe_unretained *ps = class_copyProtocolList(c, &n); for (unsigned i = 0; i < n; i++) printf("    protocol %s\n", protocol_getName(ps[i])); free(ps);
    for (NSString *k in @[@"access", @"isUsed", @"isArgument", @"tensorDataType", @"indexType", @"dimensions"]) {
        @try { id v = [o valueForKey:k]; if ([v isKindOfClass:[MTLTensorExtents class]]) { MTLTensorExtents *x = v; printf("    %s = extents rank %ld:", k.UTF8String, (long)x.rank); for (NSInteger r = 0; r < x.rank; r++) printf(" %ld", (long)[x extentAtDimensionIndex:r]); printf("\n"); }
              else printf("    %s = %s\n", k.UTF8String, [[v description] UTF8String]); } @catch (NSException *ex) { printf("    %s: (not KVC)\n", k.UTF8String); }
    }
}
int main(int argc, char **argv) { @autoreleasepool {
    id<MTLDevice> dev = MTLCreateSystemDefaultDevice(); NSError *e = nil;
    id<MTLLibrary> lib = [dev newLibraryWithURL:[NSURL fileURLWithPath:@(argv[1])] error:&e];
    MTLComputePipelineReflection *r = nil;
    id<MTLComputePipelineState> ps = [dev newComputePipelineStateWithFunction:[lib newFunctionWithName:@"k"] options:MTLPipelineOptionBindingInfo reflection:&r error:&e];
    if (!ps) { printf("pipeline %s\n", e.localizedDescription.UTF8String); return 1; }
    for (id<MTLBinding> b in r.bindings) { printf("binding %s type %ld index %ld\n", b.name.UTF8String, (long)b.type, (long)b.index); dumpProps(b); }
}}
