// Prints the name of every Metal device, one per line.
//
// It runs as its own process because the 1401 app is signed with the hardened runtime and library validation, and
// library validation refuses the NVIDIA Metal plugin (an ad-hoc signed bundle in /Library/GPUBundles). Listed from
// inside the app, the card's Metal device would be missing and the app would report the driver as not running.
// This helper loads nothing but Apple's Metal framework and the plugin, and takes no input.
#import <Metal/Metal.h>
#include <stdio.h>

int main(void)
{
    @autoreleasepool {
        for (id<MTLDevice> d in MTLCopyAllDevices()) printf("%s\n", d.name.UTF8String ? d.name.UTF8String : "");
    }
    return 0;
}
