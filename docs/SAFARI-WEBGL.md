# WebGL in Safari

Safari's graphics process (`com.apple.WebKit.GPU`) is sandboxed. Its logs show why WebGL stays blank:

    Error loading /Library/GPUBundles/NVMTLDriver.bundle/Contents/MacOS/NVMTLDriver ...
    (file system sandbox blocked mmap() of '/Library/GPUBundles/NVMTLDriver.bundle/Contents/MacOS/NVMTLDriver')

Before that, the process tried the operating-system cryptex folders, which do not hold our bundle:

    /System/Cryptexes/App/System/Library/StagedFrameworks/Safari/NVMTLDriver
    /System/Volumes/Preboot/Cryptexes/OS/Library/GPUBundles/NVMTLDriver.bundle/...

## The optional copy

`package/safari_gpu_copy.sh` places a copy of the driver bundle in the second folder. That folder is on the Preboot
volume, which is not part of the sealed system volume; the script never writes under `/System/Library` and refuses any
destination that does not resolve under `/System/Volumes/Preboot`. It is off by default:

    sudo env NULLMOTH_SAFARI_GPU_COPY=1 ./install.sh        # install with the copy
    sudo ./safari_gpu_copy.sh install                        # or add it later
    sudo ./safari_gpu_copy.sh remove                         # take it away (uninstall.sh does this too)

On a Mac where System Integrity Protection refuses the write, nothing is changed.

## What is not known

Whether the sandbox allows mapping code from that folder is not known without a run. The measurement to take on a Mac
with the copy installed (macOS 15 or 26), after a restart:

1. Open a WebGL page in Safari (for example the three.js examples) and note whether the canvas draws.
2. `log show --last 5m --predicate 'process == "com.apple.WebKit.GPU"'` and look for "file system sandbox blocked mmap()"
   against the Preboot path. If it is gone, the copy works. If it is still blocked, the sandbox allows only paths inside
   the sealed system, and the copy should stay off.
3. If the bundle maps but the process still logs "Vulkan is not up", the next blocker is the Vulkan driver files the plugin
   opens from `/Library/GPUBundles/nvmtl`; send the log with Send logs.
