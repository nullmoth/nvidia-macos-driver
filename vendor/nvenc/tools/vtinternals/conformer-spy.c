// conformer-spy.c (T8.07): DYLD_INSERT_LIBRARIES interposer that logs what VideoToolbox's pixel buffer conformer wants.
// MediaToolbox calls VTPixelBufferConformerCopyConformedPixelBuffer for every decoded frame; when the buffer does not
// conform, VideoToolbox transfers it (a Metal pass). Logged once per conformer: its attributes, its colour properties,
// whether it calls our buffer conformant, and the buffer's attributes and attachments. Arguments are forwarded as six
// pointer-sized integers (the signature is not public; x86_64 passes them in registers either way).
//   clang -dynamiclib -o conformer-spy.dylib conformer-spy.c -framework CoreFoundation -framework CoreVideo -framework VideoToolbox
#include <CoreFoundation/CoreFoundation.h>
#include <CoreVideo/CoreVideo.h>
#include <stdint.h>
#include <stdio.h>

extern uintptr_t VTPixelBufferConformerCopyConformedPixelBuffer(uintptr_t, uintptr_t, uintptr_t, uintptr_t, uintptr_t, uintptr_t);
extern CFDictionaryRef VTPixelBufferConformerGetAttributes(uintptr_t conformer);
extern CFDictionaryRef VTPixelBufferConformerGetColorProperties(uintptr_t conformer);
extern Boolean VTPixelBufferConformerIsConformantPixelBuffer(uintptr_t conformer, CVPixelBufferRef pb);

static void show(const char *what, CFTypeRef x)
{
    char buf[4096] = "(null)";
    if (x) { CFStringRef d = CFCopyDescription(x); CFStringGetCString(d, buf, sizeof buf, kCFStringEncodingUTF8); CFRelease(d); }
    for (char *c = buf; *c; c++) if (*c == '\n') *c = ' ';
    fprintf(stderr, "conformer-spy: %s: %s\n", what, buf);
}

static uintptr_t spy(uintptr_t a, uintptr_t b, uintptr_t c, uintptr_t d, uintptr_t e, uintptr_t f)
{
    static uintptr_t seen[16]; static int nseen;
    int known = 0; for (int i = 0; i < nseen; i++) known |= seen[i] == a;
    if (!known && nseen < 16) {
        seen[nseen++] = a;
        CVPixelBufferRef pb = (CVPixelBufferRef)b;
        show("conformer attributes", VTPixelBufferConformerGetAttributes(a));
        show("conformer colour properties", VTPixelBufferConformerGetColorProperties(a));
        fprintf(stderr, "conformer-spy: conformant: %d; buffer %zux%zu format 0x%08x IOSurface %p bytes/row %zu/%zu, args c=%#lx d=%#lx\n",
                (int)VTPixelBufferConformerIsConformantPixelBuffer(a, pb), CVPixelBufferGetWidth(pb), CVPixelBufferGetHeight(pb),
                (unsigned)CVPixelBufferGetPixelFormatType(pb), CVPixelBufferGetIOSurface(pb), CVPixelBufferGetBytesPerRowOfPlane(pb, 0),
                CVPixelBufferGetBytesPerRowOfPlane(pb, 1), (unsigned long)c, (unsigned long)d);
        CFDictionaryRef att = CVBufferCopyAttachments(pb, kCVAttachmentMode_ShouldPropagate);
        show("buffer attachments", att); if (att) CFRelease(att);
        CFDictionaryRef pa = CVPixelBufferCopyCreationAttributes(pb);
        show("buffer creation attributes", pa); if (pa) CFRelease(pa);
    }
    return VTPixelBufferConformerCopyConformedPixelBuffer(a, b, c, d, e, f);
}

__attribute__((used, section("__DATA,__interpose"))) static struct { const void *replacement, *original; } interposers[] = {
    { (const void *)spy, (const void *)VTPixelBufferConformerCopyConformedPixelBuffer },
};
