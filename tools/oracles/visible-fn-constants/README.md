# A function constant gating a call to a linked visible function

The shape the system's icon and Liquid Glass shaders take on macOS 26. `k` calls an extern `[[visible]]`
function, `custom_fn`, only when function constant 0 is true, and the pipeline supplies `custom_fn` through
`linkedFunctions`.

    xcrun metal -target air64-apple-macos15.0 -std=metal3.0 -c kern.metal -o kern.air
    xcrun metal -target air64-apple-macos15.0 -std=metal3.0 -c vis.metal  -o vis.air
    xcrun metallib kern.air vis.air -o vfn.metallib
    clang -arch x86_64 -mmacosx-version-min=15.0 -fobjc-arc -O1 -o vfn vfn.m -framework Foundation -framework Metal
    ./vfn vfn.metallib

Four arms. The first is the one that failed on 2.3.6: `newFunctionWithName:constantValues:` returned nil
because the driver decided whether to defer the function by reading the translator's error text, and that
text had changed. Two controls compile the call out and must pass either way. The last arm asks for a live
call with nothing linked, which has no answer and must be refused rather than run.

Measured on an RTX 5060: the 2.3.6/2.3.7-as-built plugin fails arm 1 with exactly the error the icon
processes logged; the fixed plugin passes all four and computes `custom_fn(i) = 3i + 1` for all 64 threads.
