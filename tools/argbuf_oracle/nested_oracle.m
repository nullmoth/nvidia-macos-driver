/* nested_oracle.m - sizes of a nested argument buffer as Metal's own encoders report them.
   xcrun clang -fobjc-arc tools/argbuf_oracle/nested_oracle.m -framework Metal -framework Foundation -o /tmp/nested_oracle && /tmp/nested_oracle */
#import <Metal/Metal.h>
#import <Foundation/Foundation.h>
static const char *kSrc =
"#include <metal_stdlib>\nusing namespace metal;\n"
"struct Inner { texture2d<float> t [[id(0)]]; float k [[id(1)]]; };\n"
"struct Outer { constant Inner *inner [[id(0)]]; texture2d<float> t2 [[id(1)]]; };\n"
"kernel void kn(constant Outer &o [[buffer(0)]], device float *out [[buffer(1)]], uint i [[thread_position_in_grid]]) {\n"
"  constexpr sampler s; out[i] = o.inner->t.sample(s, float2(0.5)).x + o.inner->k + o.t2.sample(s, float2(0.5)).x; }\n";
int main(void) { @autoreleasepool {
    id<MTLDevice> dev = MTLCreateSystemDefaultDevice(); NSError *e = nil;
    id<MTLLibrary> lib = [dev newLibraryWithSource:[NSString stringWithUTF8String:kSrc] options:nil error:&e]; if (!lib) { NSLog(@"lib %@", e); return 2; }
    id<MTLFunction> fn = [lib newFunctionWithName:@"kn"];
    id<MTLArgumentEncoder> outer = [fn newArgumentEncoderWithBufferIndex:0];
    printf("outer: encodedLength %lu alignment %lu\n", (unsigned long)outer.encodedLength, (unsigned long)outer.alignment);
    id<MTLArgumentEncoder> inner = [outer newArgumentEncoderForBufferAtIndex:0];
    if (!inner) { printf("nested: nil\n"); return 1; }
    printf("nested: encodedLength %lu alignment %lu\n", (unsigned long)inner.encodedLength, (unsigned long)inner.alignment);
    id<MTLArgumentEncoder> none = [outer newArgumentEncoderForBufferAtIndex:1];
    printf("member 1 (a texture, not a buffer pointer): %s\n", none ? "encoder" : "nil");
}}
