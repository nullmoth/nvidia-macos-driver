# Third-party notices

## mickys/nvenc

The vendored NVENC VideoToolbox implementation and accompanying upstream files are from [mickys/nvenc](https://github.com/mickys/nvenc), tag `v0.9.5`, commit `1e91845d6f37d8bce671d62c14f4390324da3659`. They are distributed under the MIT License; the original license text is in `LICENSE`. Upstream copyright and source notices are retained. The vendor copy does not alter upstream production source files.

## NVIDIA open-gpu-kernel-modules SDK headers

Pinned header files from NVIDIA open-gpu-kernel-modules release 610.57.04 (commit `e4a5faa2567f28c8eabe0ebb6422b6d0abcf37eb`) are in `vendor/open-gpu-kernel-modules/`. Their original SPDX/copyright headers are retained, and the complete upstream `COPYING` file is included at `vendor/open-gpu-kernel-modules/COPYING`. It states that individual files are MIT unless noted, and includes GPL-2.0 text for the resulting Linux kernel module when linked as such. This macOS library uses SDK/header declarations only; it does not build or link a Linux kernel module. Any per-file notice in an included header remains applicable.

## NVIDIA open-gpu-doc video class headers

Pinned video class headers from NVIDIA open-gpu-doc commit `9fdf5c4062007929d9f4e6cbad9c9771fe61b880` are in `vendor/open-gpu-doc/classes/video/`. The upstream MIT license text is included at `vendor/open-gpu-doc/LICENSE.md`; per-file copyright and SPDX notices are retained.

## WebKit VideoToolbox SPI declarations

`vendor/webkit-spi/CMBaseObjectSPI.h` and `vendor/webkit-spi/VTVideoDecoderSPI.h` are from WebKit commit `3bfbc200370710495ffa440abdd31b943c83ad7a`. Both headers carry the following Apple license terms, which are reproduced here for binary distributions:

```text
Copyright (C) 2020 Apple Inc. All rights reserved.

Redistribution and use in source and binary forms, with or without
modification, are permitted provided that the following conditions
are met:
1. Redistributions of source code must retain the above copyright
   notice, this list of conditions and the following disclaimer.
2. Redistributions in binary form must reproduce the above copyright
   notice, this list of conditions and the following disclaimer in the
   documentation and/or other materials provided with the distribution.

THIS SOFTWARE IS PROVIDED BY APPLE INC. AND ITS CONTRIBUTORS ``AS IS''
AND ANY EXPRESS OR IMPLIED WARRANTIES, INCLUDING, BUT NOT LIMITED TO,
THE IMPLIED WARRANTIES OF MERCHANTABILITY AND FITNESS FOR A PARTICULAR
PURPOSE ARE DISCLAIMED. IN NO EVENT SHALL APPLE INC. OR ITS CONTRIBUTORS
BE LIABLE FOR ANY DIRECT, INDIRECT, INCIDENTAL, SPECIAL, EXEMPLARY, OR
CONSEQUENTIAL DAMAGES (INCLUDING, BUT NOT LIMITED TO, PROCUREMENT OF
SUBSTITUTE GOODS OR SERVICES; LOSS OF USE, DATA, OR PROFITS; OR BUSINESS
INTERRUPTION) HOWEVER CAUSED AND ON ANY THEORY OF LIABILITY, WHETHER IN
CONTRACT, STRICT LIABILITY, OR TORT (INCLUDING NEGLIGENCE OR OTHERWISE)
ARISING IN ANY WAY OUT OF THE USE OF THIS SOFTWARE, EVEN IF ADVISED OF
THE POSSIBILITY OF SUCH DAMAGE.
```

These third-party declarations and headers are used to compile the library; they are not linked as separately built third-party binaries. `SOURCE.json` records the exact source pins and license-file locations.
