// regtest.c: build a private FigRegistry for video encoders over the plug-in folders VideoToolbox scans, and print the
// items, to see whether a third-party bundle in /Library/Video/Plug-Ins makes it in (signatures from the iOS decompile).
//   ./regtest [folder ...]
#include <CoreFoundation/CoreFoundation.h>
#include <stdio.h>

extern OSStatus FigRegistryCreate(CFAllocatorRef, CFStringRef classID, void **out);
extern OSStatus FigRegistryAddSearchPathWithOptions(void *reg, CFURLRef dir, int options);
extern OSStatus FigRegistryCopyItemList(void *reg, CFArrayRef *out);

int main(int argc, char **argv)
{
    void *reg = NULL;
    OSStatus st = FigRegistryCreate(NULL, CFSTR("com.apple.videotoolbox.videoencoder"), &reg);
    printf("FigRegistryCreate -> %d (%p)\n", (int)st, reg);
    const char *def[] = { "/System/Library/Video/Plug-Ins", "/Library/Video/Plug-Ins" };
    int n = argc > 1 ? argc - 1 : 2;
    for (int i = 0; i < n; i++) {
        const char *p = argc > 1 ? argv[i + 1] : def[i];
        CFURLRef u = CFURLCreateFromFileSystemRepresentation(NULL, (const UInt8 *)p, (CFIndex)strlen(p), true);
        printf("add %s -> %d\n", p, (int)FigRegistryAddSearchPathWithOptions(reg, u, 0));
        CFRelease(u);
    }
    CFArrayRef items = NULL;
    st = FigRegistryCopyItemList(reg, &items);
    printf("FigRegistryCopyItemList -> %d, %ld items\n", (int)st, items ? (long)CFArrayGetCount(items) : -1L);
    for (CFIndex i = 0; items && i < CFArrayGetCount(items); i++) {
        CFStringRef d = CFCopyDescription(CFArrayGetValueAtIndex(items, i));
        char b[2048]; CFStringGetCString(d, b, sizeof b, kCFStringEncodingUTF8);
        for (char *c = b; *c; c++) if (*c == '\n' || *c == '\t') *c = ' ';
        printf("  %s\n", b);
        CFRelease(d);
    }
    return 0;
}
