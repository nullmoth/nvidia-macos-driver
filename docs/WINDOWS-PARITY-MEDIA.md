# Media and power: what NVIDIA's Windows driver offers that this driver does not

Each row names the Windows feature, the state in this tree, and what closes it or why it cannot be closed.

| feature (Windows) | state here | to close, or the hard reason |
|---|---|---|
| H.264 and HEVC decode, all chips from GTX 16 | built-in decoders; vendored NVEncVT adds Ada and Ampere | Ampere H.264/HEVC through NVEncVT needs a GA10x card to validate; until then the built-in decoder stays |
| AV1 decode (RTX 30 and newer), VP9 decode | NVEncVT registers both on Ada and Ampere | Blackwell: NVEncVT does not know the Blackwell decode class; needs a Blackwell card and its class headers |
| AV1 encode (Ada, Blackwell) | Ada through NVEncVT; Blackwell none | Blackwell encode setup differs from Ada's; needs a Blackwell card to validate |
| HEVC and H.264 encode, Turing to Blackwell | Turing and Ampere through the built-in libraries, Ada through NVEncVT, Blackwell built-in | Ampere NVEncVT encode not enabled: the vendored setup targets Ada and TU117 |
| 4:2:2 decode and encode (Blackwell) | none | needs a Blackwell card |
| Several simultaneous NVENC sessions on one engine | built-in Turing library refuses a second session | the library is closed; NVEncVT covers TU117 only |
| Low-latency rate control | vendored H.264/HEVC encoders only | the built-in encoders are closed libraries |
| HDR video decode and tone mapping, RTX Video HDR, RTX Video Super Resolution | none | these are Windows driver features built on closed code; not reproducible from public sources |
| GPU idle power states | NVK per-burst boost (this branch); RM dynamic power is the kernel area's | measured idle clocks need hardware |
| Laptop dGPU power-off (Optimus RTD3 / GC6) | none | needs ACPI power-resource evaluation on a hybrid laptop; no such machine to prove it on |
| Power limit and clock lock (NVML, nvidia-smi) | none | the public 620 headers carry no control for either; RATED_TDP SET_CONTROL was reported to change nothing, and the PFM controls answer NOT_SUPPORTED to a user client |
| Fan curve, clock and voltage offsets (NVAPI) | none | no public control in the 620 headers (ctrl2080fan.h is read-side); fans on a desktop card are VBIOS-controlled |
| Telemetry (power, clocks, temperature, utilisation, throttle reasons, fan) | `tools/nvmtl-smi` (read-only, from the RUSD block, class 0xde) | a graphical overlay is not built; readings on a real card are a studio check |
| Video engine utilisation counters | `tools/nvmtl-smi` prints what RUSD reports for the video engines | the same studio check |
