#include <CoreFoundation/CoreFoundation.h>
#include <stdio.h>
#include <unistd.h>
// a VideoToolbox encoder factory that only records that VideoToolbox called it
__attribute__((visibility("default"))) OSStatus ProbeEncoder_CreateInstance(FourCharCode codec, CFAllocatorRef al, void **out)
{
    FILE *f = fopen("/tmp/vtprobe-called.txt", "a"); if (f) { fprintf(f, "called in pid %d for %.4s\n", getpid(), (char *)&codec); fclose(f); }
    return -12902;
}
