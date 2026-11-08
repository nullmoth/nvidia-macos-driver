# Changelog

## 2.8.1

- Updates no longer fail when GitHub's downloads are down. The Mac app now falls back to a copy of each release on nullmothsystems.com when a GitHub download fails, for both driver updates and its own updates. 1401 for Windows does the same for the driver package it puts on the stick. Every file is checked against the release's SHA-256 list and the NullMoth release signature, wherever it came from, and a file that differs by one byte is refused.
- The driver itself is unchanged from 2.8 (same kernel extensions, Metal plugin and Vulkan driver); only its version number moves so the app and the driver keep the same version.

## 2.8

- NVIDIA 620: the kernel driver now runs NVIDIA's 620.70.06 open kernel modules (was 615.78.08) with their GPU firmware. Two new cards come with it: RTX PRO 5500 Blackwell and RTX Spark N1X (249 device ids in all).
- NVIDIA 620: the CPU mapping hardening from 2.7.2 is carried over to 620's new mapping model. A system-memory mapping is now limited to the length the driver validated, tied to that mapping and its list, and a re-bind or close can no longer leave a mapping behind.
- NVIDIA 620: the display driver leaves the colour format to NVIDIA's modesetting, so a display mode that offers only YCbCr no longer comes up black.
- If /Library is not mounted yet when the driver starts, it loads its GPU firmware from a fallback copy.
- DLSS 4 / 4.5 (optional, RTX 20 series and newer): DLSS Super Resolution runs behind the MetalFX temporal upscaler, so a Mac game that uses MetalFX upscaling gets DLSS on an NVIDIA card. DLSS Frame Generation runs behind the MetalFX frame interpolator and takes its sizes and the game's real motion vectors from the upscaler the game attached. DLSS Ray Reconstruction runs behind the MetalFX denoised upscaler on macOS 26.
- DLSS files: the package carries no NVIDIA DLSS code. With the optional step on, the installer downloads NVIDIA's own files on your Mac, refuses any file whose SHA-256 differs from the pinned one, and compiles the DLSS GPU kernels for your card (about two minutes). Run it with `sudo env NULLMOTH_DLSS=1 ./install.sh`. RTX 20, 30, 40 and 50 series cards are covered. DLSS never runs inside WindowServer, and its data stays in a private per-user folder.
- Metal 4: command queues, command buffers, encoders, argument tables and the Metal 4 compiler work over the driver's Metal 3 objects, and the device reports the Metal 4 GPU family. MetalFX's Metal 4 upscaler and frame interpolator are served by DLSS.
- Games with ray tracing and frame generation no longer run out of video memory and crash: large automatic heaps (256 MB and up) get memory in 64 MB pieces as they are used instead of all at once. A 2 GB heap the game had made held 22 KB of data.
- Freezes in long game sessions are gone: freeing or reusing an image no longer scans every image the app ever made (hash index).
- A texture copy between two size-compatible formats is performed, as Metal allows, instead of being refused.
- Drawing into an attachment whose channel order differs from the pipeline's (BGRA into RGBA) works the way it does on Apple GPUs.
- Resources in a heap stay alive until the GPU work that uses them has finished.
- Less CPU time per draw: parallel render encoders record strided vertex buffers and heaps directly, and a buffer's GPU address is returned without a lock when it is already known.
- Fewer stalls: the Vulkan layer carves command memory from shared 1 MB slabs and reuses it across command pools instead of opening a kernel connection per pool.
- Ray tracing in Cyberpunk 2077: the game offers ray tracing only when the reported working set is above 6145 MB. On a card with at least 7.2 GB of memory the driver answers 6200 MB while ray tracing is available; a smaller card keeps its own answer, so it is never told it holds more than it has.
- Cyberpunk 2077 (ARM apps translation) is told macOS 27 so it offers MetalFX frame generation, which the driver provides on macOS 15 and 26. Measured on an RTX 5060 at 1080p, all ray tracing on, DLSS upscaling and frame generation: 85.84 fps average, 68.18 fps minimum (was a crash, or about 47 fps without ray tracing in 2.7.2). Geekbench 7 Metal on the same card: 116,053.
- Shaders: an atomic on an unindexed local array works, and translation is faster (each shader is parsed once).
- Managed no-copy buffers of every size get a copy in video memory.
- OpenGL: the uniform buffer reads its slot through the handle check.
- If Cyberpunk 2077 had ray tracing or frame generation greyed out or off before, turn Ray Tracing and Frame Generation back on once in its graphics settings: the game remembers its own earlier answer.
- Known issue: some interiors in Cyberpunk 2077 can render too bright on the first launch after a restart. Quit and launch again. A fix is planned for 2.8.1.

## 2.7.2

- Apps built for macOS 26 that read a pipeline's required threadgroup size no longer quit with an "unrecognized selector" error. Compute pipelines report the size they were created with, and a required size also sets the pipeline's maximum threads per threadgroup.
- The depth bounds test works. Before, setting depth bounds on a render encoder did nothing, so pixels that should have been skipped were drawn. Pixels are now skipped when the depth already stored in the depth attachment is outside the bounds, as on Apple GPUs.
- Texture views can be made from a texture view descriptor (macOS 26), with a pixel format, mip levels, slices, and a swizzle.
- Setting a colour attachment map on a Metal 3 render encoder no longer stops the app. As on Apple GPUs, the map has no effect on a Metal 3 pipeline.
- Fragment barycentrics are reported only on GPUs whose Vulkan driver provides them, so an app never builds a pipeline the GPU cannot run. On AMD GCN 3 and 4 the Vulkan driver now uses half-precision arithmetic where the hardware has it.
- The 2.x driver is built for macOS 15 and macOS 26. On any other macOS version it creates no Metal device.
- AMD and Intel GPU driver (test program, opt-in): AMD GPUs no longer stop at start-up after "register mmio size" (a PCI bridge lookup could loop forever). Safe start-up for RDNA 2, 3 and 3.5, GCN 1 to 3, Polaris cards macOS has no driver for, and Vega APUs. AMD GPUs that Apple's own drivers already run are left alone.
- Security: fixed races in the NVIDIA kernel driver that let a logged-in user crash the Mac or corrupt kernel memory by binding, mapping, and closing GPU connections at the same time.
- Security: the display driver no longer publishes kernel addresses where any app can read them.
- Security: GPU profiling and channel priority now need an administrator. They were open to every user.
- Security: a setuid or root process ignores the driver's environment settings, and a release build only loads a library named by one from a root-only folder.

## 2.7.1

- OpenGL apps: setting the same render pipeline again inside an open render pass is skipped, so draw-heavy OpenGL apps spend less CPU per draw.
- ARM apps: the installer keeps /Library/NullMoth readable, so the ARM translator can be reached without the `sudo chmod 711 /Library/NullMoth` workaround.
- Known issue: Cyberpunk 2077 can still render interiors too bright on some launches. Quit and launch the game again.

## 2.7

- ARM apps (opt-in, experimental): an optional translator for Mac apps built only for Apple silicon. It compiles an app's ARM code into Intel (x86-64) code ahead of time, so the app can run on Intel and AMD hackintoshes. Turn it on in 1401 › ARM apps (opt-in); on Windows, tick Also set up ARM apps (opt-in) on a fresh install. Your original app is never changed, and Turn off removes the translator and every translated copy.
- ARM apps works today for command-line tools (lz4, xxhsum, cmake and z3 give byte-identical output to Apple silicon). Apps with windows are experimental; Go, Flutter, Java/Electron and SVE video codecs do not work yet, and games are not supported in 2.7. It needs a CPU with AES instructions and the Xcode Command Line Tools.
- macOS 26 Tahoe has sound. Tahoe ships no HD Audio driver, so PCs on it had no output device at all. The driver package now carries NullMoth's own HD Audio driver and installs it on Tahoe when your PC has an HD Audio controller: onboard Realtek and other codecs (rear and front analog) and NVIDIA HDMI/DisplayPort audio. macOS 15 keeps Apple's own driver and is not touched.
- If Logic Pro, GarageBand, or MainStage says "Selected Driver not found" after updating, open its Settings › Audio and pick your output again. The old device was Apple's, and it no longer exists.
- Kernel panics fixed. The driver kept a saved interrupt state inside a shared lock object, which macOS reported as "element modified after free" and which could end in a panic. The state now stays with the caller, as NVIDIA's code expects.
- Games that run several compute passes at once no longer hit random GPU faults (FAULT_PTE / FAULT_PDE) or show garbage lighting. Concurrent compute work and render passes are now ordered correctly.
- Resources declared with useHeap: and residency sets stay alive until the GPU is done with them. Before, a heap could be freed under work that still used it.
- Freed buffers, images, and views keep their GPU address until the work that was in flight when they died has finished.
- Long sessions no longer run out of GPU connections: the command-pool cache is bounded.
- The memory a game is told it may use leaves a reserve on the card, so big games run out of VRAM less often.
- A shader that uses a heap binding its heap was not created with is refused instead of running into a GPU fault.
- Metal ray tracing: acceleration structures build on the GPU, instance transforms honour the matrix layout the app declares (before, every column-major transform was wrong), builds are thread-safe, and VRAM is checked before a build.
- Bindless argument-buffer heaps carry storage images and texel buffers, so a shader can write a texture it found through a heap.
- 10-bit BGR10A2 textures can be written by compute shaders (HDR and 10-bit output render correctly).
- sRGB images keep their storage use through a compatible UNORM view, so writes to them land.
- A render pipeline that does not match the open render pass restarts the pass with its attachments kept, instead of the draw being dropped.
- Indirect compute dispatches prepare the textures declared with useResource:usage: first.
- Shader translation: make_filled_simdgroup_matrix, barrier scopes, byte-addressed threadgroup memory, pointer phis, device-address arithmetic, and pixel-coordinate sampling now translate correctly.
- Functions that call visible functions translate once their callees are known.
- Application profiles: the driver applies per-application settings and shader fixes for one named application, in that process only, with its own shader cache. Cyberpunk 2077's light clustering now renders correctly at about 40 fps on an RTX 5060.
- GPU compute: convolution kernels use the tensor cores, with a speed gate that keeps Apple's MPS kernel wherever ours is not clearly faster.
- Shader compiler: small per-thread arrays stay in registers, plus better scheduling, conversion, and load handling.
- Compute pipelines skip buffer bounds checks (graphics keep them), and small private buffers and images share VRAM blocks.
- Less CPU per draw and dispatch: binding tables in fixed arrays, fresh system memory is not zeroed twice, and buffers are parked for reuse.
- Diagnostics that cost speed are compiled out of the release build.
- Known issue: Cyberpunk 2077's first launch after a restart can render interiors too bright while the game streams its files in. Quit and launch the game again.

## 2.6.6

- Launchpad and Chrome icons are no longer black. 2.6.5 changed how textures made from an IOSurface with no stated use were set up, and shader writes to them went nowhere (PR #461, thanks Geek).
- Logic Pro no longer crashes when it opens the Dolby Atmos mixer. A texture upload reached the GPU with no image behind it; the copy is now refused, and the log names the texture so the cause can be found.
- QQ: avatars keep their colour (a clear that finished late could wipe a new upload, PR #458, thanks Geek) and SVG toolbar icons draw (the shader translator refused ANGLE shaders with an empty inline-assembly statement, PR #457, thanks GeekAudio).
- Hades menu text draws: the shader translator kept a pointer read through a branch merge (PR #466, thanks mickys).
- Games that ask for rasterization-rate-map coordinates, such as Darkest Dungeon II, get their shaders: the driver reports that it has no rate maps and the translator passes those coordinates through unchanged (PR #469, thanks mickys and Geek).
- A failed buffer-to-texture copy names its format, sizes and region in the log (PR #465, thanks mickys).
- The installer notes when the boot-args have no AMFI argument. Without one, WindowServer refuses the Metal driver and the desktop comes up with no Metal device.
- Tahoe PCs whose integrated GPU drives a screen no longer stop at a black screen with a cursor: a GPU macOS no longer accelerates does not count as the one showing the desktop, so WindowServer starts.
- macOS Sequoia 15.0 to 15.4 can install the driver again. The Vulkan library in 2.6.2 to 2.6.5 was marked as needing 15.5 though nothing in it does.
- Security: the driver package is signed, and the Mac app installs a driver update only with a valid NullMoth signature. An installer rollback no longer reuses a firmware folder another account could have placed in /Users/Shared.

## 2.6.5

- WebGL pages with shadow-casting lights draw their shadows: when a page's shadow helper read its sampler through ANGLE's texture table, the shader translator refused the shader, and a second light's shadow map was read as empty (#449, thanks Luminous418).
- WindowServer does less work per frame: compositor draws no longer rewrite descriptors for the framebuffer-fetch set (PR #439), and command buffers whose completion slot was reused are recognised instead of shutting down the pool (PR #446). Drawables are presented on their own queue, which ends a white-window hang with the Metal HUD on (PR #445). Screen recordings keep their frames in order (PR #438). Thanks GeekAudio (mickys).
- Photo Booth's preview is no longer black (PR #433, thanks daggersinmybackk).
- Vivaldi, Opera and Arc get Metal through ANGLE, found by the libraries the process loads (PR #450, thanks SameerAlSahab).
- Shader translator: depth gather-compare (Civ VI shadows, PR #447), sampler choices across compare chains (PR #448), cube samples through 2D-array views (Stellaris, PR #434), Apple's GL helpers inlined (PR #435). Thanks GeekAudio (mickys).
- Plugin: Apple's ARGB8 texture format for sampling and uploads (Homeworld, PR #440), depth texture types and sizes reported correctly (Northgard, Dome Keeper, PRs #441 and #432), array lengths kept in struct reflection and encoders return their command buffer (Hades, PRs #437 and #436), block-aligned compressed mip copies (Cult of the Lamb, PR #444), uploads ordered after GPU reads (PR #442), and a trace-ID lock conflict removed (Soulstone Survivors, PR #443). Thanks GeekAudio (mickys).

## 2.6.4

- three.js scenes with two or more shadow-casting lights no longer lose their WebGL2 context in Chrome, Brave and other browsers: the shader translator refused a shader's array of samplers (#426, patch by Luminous418). Each sampler in the array now reaches the shader with its own state. Known: WebGL shadow maps still draw the whole shadowed surface dark, with one light or several (it did in 2.6.3 too); that is the next fix.
- Arrays of textures read the right texture: every element past the first read the first one, whether the index was fixed or computed in the shader. Shadow maps, texture atlases and material arrays in games and WebGL pages were affected.
- Machinarium, and other apps whose shaders were built with older Metal tools, draw their scenes: the translator did not recognise a sampler written directly into the shader code, and refused the shader (patch by daggersinmybackk).

## 2.6.3

- Infuse and other players that scale video with Apple's Lanczos filter play again: the shader that filter uses was refused, and the app stopped with "Unable to get MPS kernel lanczosVerticalScale" (#405). Its output now matches a real Mac's pixel for pixel.
- Geekbench's GPU benchmark runs every test, including the machine-learning ones that use depthwise convolution (#377, PR #385, thanks GeekAudio and Geek).
- Unity WebGL games and other web pages whose shaders pass a buffer to a helper function render again (#406). Chrome no longer loses its WebGL2 context on pages that write only to textures (#419, patch by eisda).
- DaVinci Resolve no longer stops at startup on a GTX 1660 Ti: the shader translator read debug notes in Apple's shader code as part of the instructions (PR #418, thanks fayzetwin1).
- llama.cpp gets correct results instead of zeros (#401).
- Dragging windows and copying between apps no longer stalls on cards with a small 256 MB BAR1 window: surfaces keep the memory they were given between copies and are read back directly (#368, #399, patch by mrtzygt). A setting you put in your own block of nvrm610.conf now wins over the shipped value.
- Window dragging no longer stutters, and a crash when a window closed during a copy is fixed (#251, #340, #412). The Dock and Control Center blur is smooth instead of blocky (#363). HiDPI "looks like" sizes appear in Displays (#354). Scrolling in Chromium apps is smoother, and System Settings panes open faster. These come from watchthelight's NvGfx (thanks watchthelight).
- Display rotation in System Settings (#230, #307, #350, #355, PR #398, thanks razvanccr).
- HDMI and DisplayPort audio follows the right display when several are connected, and comes back after a mode change (PR #366, thanks Ersiter).
- A laptop's built-in screen keeps the brightness you set across restarts and power changes (PR #312, thanks Ersiter).
- Activity Monitor shows how busy the NVIDIA GPU is (PR #380, thanks GeekAudio).
- Boards whose firmware gives the GPU no 64-bit window keep BAR1 where the firmware put it, and a BAR1 of 512 MB or more that was not moved can flip (PRs #408 and #409, thanks mischa85).
- Microsoft Edge and other Chromium browsers get Metal through ANGLE by name (PR #404, thanks DoveKi).
- Minecraft shader packs BSL and Complementary work, and OpenGL apps use uniform buffers (PR #343, thanks Dat 'Del4y' Nguyen).
- The shader translator is faster (PR #187, thanks modest-tao).

## 2.6.2

- The same driver as 2.6.0, released with fixes to the 1401 apps for Mac and Windows.

## 2.6.1

- The same driver as 2.6.0, released with the redesigned 1401 apps for Mac and Windows.

## 2.6.0

- An update of the driver takes one restart. macOS shows a notice that the extensions were updated while the installer replaces them, and the installer used to answer it with "allow the extensions, then reboot once more"; the new driver already loads on the first restart (measured: every extension approved, none held back), so on an update it now says one restart is enough. A first install still asks you to allow the extensions once.

## 2.5.5

- Discord videos play again. 2.5.4 stopped registering the hardware video decoder in every sandboxed process, and Discord decodes inside its own sandboxed GPU process, so its videos did not play at all. Only Apple's sandboxed apps (Safari, QuickTime, Preview) now hand decoding to macOS's video decoding service; Chromium and Electron apps decode in their own process as in 2.5.3.
- Remote-desktop apps such as UU Remote show the picture: the hardware H.264 and HEVC encoders marked every keyframe "not a sync frame: false" where Apple's encoders leave the mark out, and apps that look for the mark's absence never found a keyframe on every card generation (#388, #358, thanks a1461750564 and sherves for measuring it). The encoders for the newest cards also load on macOS 15.0 to 15.4 now; they asked for 15.5.
- Screen Sharing and other screen-capture apps show the screen instead of black: a texture made from a buffer could only be viewed as its own format and its sRGB twin, and screen capture views a BGRA surface as RGBA when it scales or converts.
- Brave and Microsoft Edge get Metal through ANGLE the way Chrome does (PR #381, thanks SameerAlSahab; Edge tested by sherves).
- On a card with a small BAR1 window, NVAccel no longer logs every refused allocation: a third of all log lines on an idle desktop (#387, thanks wwwljxw). It logs the first eight and then every 256th with the running count, as the framebuffer does; `debug.nvaccel_vm_refused` keeps the exact total.

## 2.5.4

- Desktops whose only graphics card is the NVIDIA card no longer loop on a black screen when the card's display does not come up: WindowServer used to stop at start with "Failed to create MetalDevice" every ten seconds until the startup guard switched the driver off. It now starts on the firmware's framebuffer, the desktop appears without acceleration, and the first-login report sends the logs that show why the display did not come up.
- Discord gets Metal through ANGLE the way Chrome does (PR #348, thanks lukeitsback).
- Safari, QuickTime, Preview and other sandboxed apps decode H.264 and HEVC through macOS's video decoding service instead of failing inside their sandbox (PR #349, thanks lukeitsback).
- The installer accepts a /Library folder whose access-control list only takes access away, and names any entry that would let another account write there.
- When a kext you added to /Library/Extensions stops macOS from building its kernel collection, the installer names that kext and says how to move it, instead of only "test kernel collection build failed".
- The package carries the licence texts of everything in it (NVIDIA's firmware licence, the open kernel modules' licence and Mesa's), installed to /Library/NullMoth/licenses; the uninstaller removes them.

## 2.5.3

- NVIDIA's driver options given as `nvregdwords=` no longer stop the startup. From 2.5.0 to 2.5.2 any option set this way ended the boot with a kernel panic in the driver; the options now reach NVIDIA's registry, as the 2.5.0 notes describe.
- HDMI and DisplayPort audio, stated precisely: on macOS 15 the driver gives AppleHDA the display's audio link, and the display shows up under Sound > Output. macOS 26 removed AppleHDA, so on macOS 26 this package does not provide display audio by itself.

## 2.5.2

- HDMI and DisplayPort audio: a connected display shows up under Sound > Output with no setup. The driver publishes the display-to-codec link AppleHDA needs, keeps it current through hot-plug and monitor swaps, and lets a machine whose display audio is on another codec pin name it (PR #284, thanks Hi-Jiajun; PR #313, thanks Ersiter).
- Laptops whose NVIDIA bring-up takes longer than 40 seconds keep their desktop: the startup hold waits while the GPU is still coming up, up to 120 seconds (`nvrmholdcap=<seconds>` sets it) (PR #317, thanks watchthelight).
- Machines with several NVIDIA cards can pin the desktop to the card that drives the boot screen, and the choice survives updates (opt-in, see docs/AUTO-PRIMARY-GPU.md; PR #247, thanks is-pan).
- High-refresh displays: `sudo sysctl debug.nvrmfb_flip_latch=2` checks each flip at the next present instead of waiting for it, giving back about half a refresh period per frame (opt-in; #300, thanks xiaoming-crypto).
- The system log is quiet again: the driver logs failed GPU allocations, not every successful one (#242).
- Apps using Vision barcode detection (QQ and others) and Geekbench's Background Blur no longer sit at full CPU for minutes compiling one MPS kernel (#334, #111). That kernel still does not run on the GPU yet.


## 2.5.1

- Apple TV plays video again, and dragging windows no longer kills apps like Telegram: CoreAnimation's extended-range video formats (BGRA10_XR / PBGRAXb and BGR10_XR) are supported; the driver refused them, so video and some window pipelines failed (#309, #280).
- Shader writes to sRGB textures land, encoded as on a real Mac; they were silently dropped, which broke Blender's edit-mode viewport (#321).
- Scaled and HiDPI desktop modes work: a desktop size different from the display's timings is scaled by the GPU's head scaler instead of being refused (PR #273, thanks Leaf-lsgtky and lsCoding666, #221).
- Faster window updates: CPU-drawn surfaces upload from their own pages through the copy engine (PR #296, thanks mickys).
- GPU temperature in iStat and other monitors on cards whose sensor register answers (tested on Ampere; PR #323, thanks GeekAudio).

## 2.5.0

- Built on NVIDIA's 615.78.08 driver (from 610.57.04): NVIDIA's own kernel driver, display code and GSP firmware for every card, with all of NVIDIA's fixes since 610, including the blocky corruption in The Last of Us Part II.
- New cards: NVIDIA T400, T500, T550, T600, T1000 and T1200 (desktop and laptop), and the second RTX 5070 device ID.
- The GPU's identity now comes from the PDI NVIDIA stores in the card's PCIe configuration space, the way NVIDIA's driver reads it; older boards fall back to the previous method.
- NVIDIA's driver options work on macOS: the boot-arg `nvregdwords=Key=Value;Key=Value` is NVIDIA's `NVreg_RegistryDwords`, read by NVIDIA's own parser. `nvregdwords=RmDisableDisplayGlitchPerfLimit=1` is NVIDIA's new lower idle power with several monitors (it can glitch the display briefly when clocks change).
- 12-bit HDMI deep colour: the boot-arg `nvmaxbpc=12` allows it on displays that report it. The default stays at 10 bits, as NVIDIA ships it.

## 2.4.4

- Hardware H.264 and HEVC encode on every GTX 16 card: GTX 1650, 1650 SUPER, 1660, 1660 SUPER and 1660 Ti use the same encoders as RTX 20.
- HEVC encode on GTX 16 and RTX 20 cards keeps going past the second frame; it stopped with an error from frame 2 on.
- Laptops whose screen is on the Intel or AMD GPU offer the NVIDIA Metal device about 8 seconds after startup instead of 60, so apps opened at login see it too.
- WindowServer no longer crashes in the Vulkan driver when a binding holds a handle that cannot be a live object: the slot gets a placeholder and the log names it. Two RTX 3060 machines restarted WindowServer a few seconds after each login on this.
- An app no longer freezes for good when one finished GPU job is missing its completion mark (#322): Terminal and iTerm2 printing fast stopped redrawing and taking input until force-quit.
- Shaders that call functions through a visible or intersection function table load again; they returned nothing since 2.3.6 (PR #318, thanks watchthelight).
- World of Warcraft starts again: since 2.3.6 it showed a black screen and "unable to start up 3D acceleration", and once past that the terrain lighting shader faulted (PR #304, thanks razvanccr).
- Apps built on wgpu (Tern and others) start again: a fragment shader that clears the padding of a local struct failed to translate and the app stopped at launch (#330).

## 2.4.3

- Fix a desktop freeze followed by a kernel panic ("thread_call group 'high' reached max thread cap") a few seconds to a few minutes after logging in (#295). The display driver started a new kernel thread for every vblank and every cursor move while an earlier one was still waiting; each of its callbacks now runs one at a time.
- Metal apps translate a shader they have already seen without hashing it again: the digest kept on the translated text was never reused (PR #294, thanks watchthelight).

## 2.4.2

- Fix the black screen and the startup that stops on the verbose text after updating to 2.4 or 2.4.1. The display driver checks for a part of the display interface that the 2.4 and 2.4.1 core driver was built without, so it refused to start and the screen never moved off the startup text. 2.4.2 builds the core driver with it again; nothing else changes.

## 2.4.1

- Fix a black screen after updating to 2.4. The 2.4 package carried a framebuffer kext built from an outdated copy of its source, missing the display code the earlier releases had; 2.4.1 ships the complete framebuffer with the 2.4 changes.

## 2.4

- Hardware H.264 and HEVC video encode on RTX 20 (Turing) and RTX 30 (Ampere) cards, selected automatically per card; earlier builds used the hardware encoder on RTX 50 only.
- Security hardening: caller values are bounded, each client is serialised, and kernel addresses, registers and firmware stay private.
- macOS 26: the system icons and the glass menus and panels draw correctly again.
- Install from a root-owned copy of the package, with the previous driver kept as an automatic fallback.
- The kernel driver reads the power source (battery or mains) and reports it to the GPU firmware.
- Clear a stale shader cache left by an earlier build, which could show a black screen at login after an update.
- A boot counts as good once the desktop is up.

## 2.3.6

Desktop and windows
- Dragging and scrolling windows: a surface whose video memory had been set aside is put back before its update is
  recorded, so the driver no longer copies the whole window back to system memory on every frame (PR #245, thanks
  HighDelay; #253 measured the same readback on an RTX 5070 Ti at 4K)
- Moving the mouse no longer stalls the compositor: the cursor position is applied on its own thread instead of under
  the system's cursor lock, and OpenGL apps wait for the display's real refresh rate instead of 60 Hz on faster
  panels (thanks pengubow)
- Core Animation passes that read the colour attachment with framebuffer fetch off now read a defined image instead of an
  unbound descriptor, which could put garbage on screen inside WindowServer (PR #270, thanks watchthelight)
- Text and badge masks drawn into A8 targets land in the right lane, and Dock badges show their numbers again (PRs #271
  and #274, thanks watchthelight)

Apps and games
- OpenGL: Minecraft's vertex shaders translate, the draw path no longer allocates per draw, texel-buffer descriptors and
  unchanged bindings are reused, depth bias and per-instance step rates take effect, samplers work inside argument buffers
  (thanks HighDelay)
- Chrome and Chromium use Metal through ANGLE by default, so their GPU process starts; Electron apps stay opt-in with
  NVMTL_ANGLE_METAL=1, and NVMTL_ANGLE_METAL=0 turns it off everywhere (PR #263, thanks Hi-Jiajun)
- Shaders with `[[visible]]` function references that function constants turn off are no longer refused, which lost
  pipelines in the login window, Spotlight, icon services and the Dock
- Metal reports the card's whole video memory as its working set, as a Mac does (8 GB cards said 5.9 GB, 12 GB cards 8.9 GB)

## 2.3.5

Stability
- The login screen no longer stays black for a minute after the display comes up. The driver waited 60 seconds before
  letting the compositor's frames reach the panel, on the assumption that the screen was already showing them another
  way; on macOS 26 it was not, so every boot sat on a black login screen for exactly that minute

## 2.3.4

Pre-release for the early-updates testers; it becomes the general release only after every GPU generation passes the
field gate.

Stability
- Window dragging, Launchpad and games stutter less: each screen update waits at most one refresh period for the
  previous one, instead of a fixed tenth of a second; the wait wakes on the display's own event
- macOS Sequoia screenshots and window captures: the capture ordering fix from 2.3.2 returns, and it now follows what the
  installed kernel driver supports, so a Metal driver and a kernel driver from different versions never mismatch again
- Turing (GTX 16 and RTX 20): the login screen no longer stays black with a cursor when the first frame has not been
  drawn yet; the driver only shows video memory the Metal driver has written
- Turing: the ten-second start wait the installer used to add is now the driver's own default, so a laptop card or a Mac
  whose boot arguments were rewritten gets it too (a value you set yourself still wins)
- Laptops with a hybrid display: the driver no longer copies the boot screen from another card's memory, which could
  stop the Mac during startup. When WhateverGreen is present next to the driver, the installer notes the gfxrst setting it needs
- A Mac reset during the driver's own start-up wait (the ten-second Turing settle, the 100-second wait when the card's
  memory window cannot be moved) no longer counts toward switching the driver off. Two such resets used to leave a
  working Mac on the fallback driver, reported at the next login as "driver loaded: no"
- A GPU that is busy is not a GPU that is gone: the Metal driver keeps waiting for a late frame instead of disabling
  itself for the rest of the process. In WindowServer that disablement meant every later command buffer was refused
  and the session restarted ("Failed to get a MTLCommandBuffer")
- Every kernel driver now carries the package version. A Mac that still runs an older kernel driver after an update
  (the kernel collection was not rebuilt) is named in Send logs and in the "reached the desktop" report instead of
  counting as the new version

Logging (so a report says what happened)
- The kernel driver logs its own version at start
- A full video-memory window logs the first refusals and then every 256th, with one line saying who holds the window
- Spare framebuffer instances (more instances than displays) say so instead of reporting a failure
- A display that stops flipping logs how long the frame had been pending and the flip counters
- Every ten seconds of screen activity, one line per display records the flip rate against the panel rate, missed
  refreshes and skipped frames
- A refused mode set or flip in the kernel names the stage that refused it
- The Metal driver says why it could not begin a command buffer (a lost GPU, a failed allocation), the line
  WindowServer crashes without

## 2.3.3

Stability
- 2.3.2 made some Macs worse: blank or torn windows and a Dock missing its apps on macOS Sequoia, WindowServer
  crashes on some Tahoe desktops. Its display changes (screenshots and the lock screen, the brightness and video-memory
  logging, the Turing flip change) are withdrawn from the package until they pass a staged test with the testers.
  2.3.3 ships the 2.3.1 driver, which has the longest clean record in the field, on both macOS 15 and macOS 26
- macOS Sequoia: installing and updating works again (the 2.3.2 fix is kept: the Metal driver is built for macOS 15)
- After a rollback, the installed version record follows the files that are really installed (kept from 2.3.2)

## 2.3.2

Fixes
- macOS Sequoia: installing or updating to 2.3.0 or 2.3.1 stopped with "runtime payload requires macOS 26.0.0". The
  Metal driver in those packages was built for macOS 26 only; it is built for macOS 15 again, and a release can no
  longer ship anything that needs a newer macOS than 15
- Screenshots, window captures and the lock screen showed a blurred or empty window. macOS reads a window right
  after drawing it, and the driver now finishes the drawing first, as Apple's drivers do
- Send logs now includes how much of the card's video memory window is in use, so a black screen with only a
  cursor can be told apart from a leak
- After an update was rolled back, Send logs still named the new version. The installed version now follows the files
  that are actually installed, so reports say what your Mac is really running
- The Metal driver never hands the GPU a missing texture or sampler; it uses an empty one and logs it (a cause of
  Safari crashes in older versions)
- Laptops with the screen on the NVIDIA GPU: every step of the brightness setup is logged, so brightness that stays
  greyed out can be traced from Send logs

AMD / Intel test program
- A test driver that fails to start no longer panics the Mac: memory still mapped when the start gave up is kept
  until it is unmapped, instead of being freed under the mapping
- Intel 11th and 12th gen graphics (Tiger Lake, Rocket Lake, Alder Lake-S) and DG1 now reach the Intel driver. Two
  drivers list these chips and the device was offered to the wrong one, so neither took it
- When a start fails, its first error lines are kept and reported on the next start, even if the Mac stopped
- The test driver's log names AMD and Intel devices correctly

## 2.3.1

Safer updates
- If an update cannot bring your display up, your Mac no longer stays on a black screen. After two starts in a row
  that never reached the display, the driver turns itself off so macOS still starts, and the driver you had before the
  update is put back automatically with one restart. Nothing stays running once an update has worked
- Releases now also have to pass app checks on the new Metal driver (Terminal text, window capture) and may not do
  worse than the release before

## 2.3.0

Fixes
- Plugging a different monitor into the same port: macOS sees the new monitor (its name, size and modes) instead
  of keeping the one it replaced
- Terminal and other apps that draw text with programmable blending: text no longer comes out missing or garbled
  (GTX 16 and others). Draws that read colour from a pass without every attachment were skipped; they are now drawn,
  and overlapping triangles in one draw read each other's colour in order (thanks pengubow)
- Above 60 Hz: the display and the accelerator woke up once per millisecond to check for a finished flip, which
  users' flicker recordings showed as about 56 flips a second on a 143 Hz panel while windows moved. They now wake on
  the flip itself (the display on macOS 15 and 26, the accelerator on macOS 26)
- When a display's mode set is refused, the driver now tries the mode the firmware is already running before giving
  up, and the log says why the mode was refused (reports of a black screen at login on some RTX 3060 boards)
- Draws the Metal driver had to drop are now in the logs that Send logs collects

AMD / Intel test program
- 1401 can join the AMD and Intel GPU test program (AMD/Intel panel, Join). Testers get the newest signed test build
  of the AMD/Intel driver, and after every restart 1401 sends what the driver did, so a start that hangs or panics
  still reaches us. Leave the program from the same panel at any time

## 2.2.0

This release makes the driver do one thing: drive NVIDIA GPUs. Everything else it used to carry is now its own
product or is gone, which makes the package smaller, leaves less running on your Mac, and leaves less to go wrong.

Leaner
- The package carries the NVIDIA driver only: the AMD/Intel GPU driver, the NPU driver and the audio driver are
  separate products with their own packages and installers (the package drops from about 1 GB)
- The boot logo is gone: the OpenCore logo driver, the boot progress screen and the display driver's code that kept
  the logo on screen. Installing, updating or removing the driver takes an existing logo out of OpenCore
- Three background jobs are gone: a display tracker that streamed the driver's log all the time, a WindowServer
  restart at every boot (a flicker workaround from before the flicker fix in 2.1.0), and a crash-check login item.
  Logs are collected only when you press Send logs. Installing or removing the driver takes out what an earlier
  version left
- The 1401 Mac app moved to the 1401 project; it installs this driver with the package's own installer

Fixes
- A display whose firmware boot timing NVIDIA's display code does not accept (seen on an RTX 3060 at 1920x1080) now
  switches to the validated timing for the same resolution instead of crashing WindowServer at login
- Checksum lists are checked strictly: a malformed line in SHA256SUMS used to be skipped while the check still passed
- install.sh fixes PATH before running anything as root, like uninstall.sh
- Crash reports sent with Send logs keep the library identifiers needed to read them (machine identifiers are still
  removed)

## 2.1.0

Driver
- Each GPU generation (GTX 16, RTX 20, 30, 40 and 50) is now described by the card itself: the driver reads the
  architecture and the newest engine classes from the GPU and publishes them, and the Metal driver picks the video
  decoder and the scan-out model from that instead of a table of device IDs
- Cards with a small BAR1 (Resizable BAR off, or a board that does not offer it): the driver moves BAR1 away from the
  boot screen at its own size, so GTX 16 / Turing cards get the same zero-copy display path as the others. A BAR1 that
  macOS left unassigned is reported instead of failing with no reason
- The desktop no longer waits for the GPU on every frame (#196): each window the compositor draws is marked when the
  GPU finishes it, and the display waits only for that mark
- A BAR1 the firmware already placed large is left as it is, and the old setup comes back if a resize fails
- Gamma tables are applied; HiDPI desktop modes can be turned on with nvfbhidpi
- The installer keeps your own settings block in nvrm610.conf

Metal
- Blender: Eevee renders lit scenes. NVIDIA GPUs cannot write the stencil value from a shader, which Blender's Metal
  backend relies on to sort its lighting; the driver now rebuilds that value by drawing once per stencil bit. Also
  fixed: a light loop that hung the GPU (the first active SIMD lane, and loop values lost when helpers were inlined),
  render passes with no attachments, layered 3D colour targets, framebuffer fetch, samplers held in argument buffers,
  and image atomics in helper functions. Blender's shadow pages no longer crash it
- Indirect compute dispatches read their grid size on the GPU instead of stopping to read it on the CPU
- Hardware video decoders are registered with macOS's decode service (#175)

AMD / Intel test driver
- The log now says which driver started, which PCI drivers registered and which device each one matched, so a GPU
  no driver took can be traced from a user's log

Mac app
- Map my GPU (experimental, opt-in)

## 2.0.4

Mac app
- Prepare this Mac for Tahoe is back: on macOS 15 one button installs or updates the driver (it carries the Tahoe
  driver too), parks it and sets the installer's GPU settings, then opens Software Update. After the update the
  first start sets the driver up for Tahoe and restarts once by itself. Cancel puts everything back, also with a 1401
  stick plugged in next to the internal disk (it changes back the config the preparation changed)
- Intel and AMD laptops and PCs whose own GPU OpenCore switches off (disable-gpu, or -wegnoigpu on Intel) can try the
  AMD / Intel test driver: the app no longer says "no GPU" and stops; turning the test driver on turns that GPU back
  on (also on Core Ultra, where the NPU used to hide it), and turning it off switches it off again exactly as it was

1401 for Windows
- When the stick's disk number is gone at write time, the log now lists the USB disks Windows shows instead

## 2.0.3

Driver
- System sleep and wake (PR #182, thanks danlt150): the GPU's memory is kept across sleep, the PCI setup and
  Resizable BAR are restored on wake, and the display driver does the wake steps macOS expects. Before, the GPU
  lost its state in sleep and the desktop could freeze on wake
- The Metal driver finds a submission's copy buffers once instead of on every check (PR #223, thanks HighDelay)
- The display driver no longer fills the log with the same "hdcs" line many times a second (#216)
- Security (#214): diagnostic sysctls that let any user read physical addresses and memory are gone from normal
  builds (they exist only with the nvrmdiag=1 boot argument, and only root can read them); starting the GPU by
  hand is root-only; the kernel addresses used for debugging are left out of shipped builds

Mac app
- AMD boards (AM4 and AM5) whose OpenCore config rewrites the firmware memory map get DisableVariableWrite, so
  macOS cannot write NVRAM through that map (boards that stopped starting after an install)
- When no NVIDIA card is visible, the app says why: -wegnoegpu or -wegonogpu in boot-args, a disable-gpu property,
  or a laptop BIOS set to integrated graphics only
- Macs with only an AMD or Intel GPU can turn on the experimental AMD / Intel GPU driver; it installs from the
  downloaded driver package, without the NVIDIA driver
- Laptops whose built-in screen is wired to the NVIDIA GPU get an opt-in switch for brightness control

1401 for Windows
- Intel PCs (12th gen and newer) that stop at EB.MM.AKM because no kernel slide fit: rebuilding with the stick
  plugged in reads its startup log and turns on DevirtualiseMmio for the next start
- AMD PCs get the same NVRAM write guard as the Mac app (DisableVariableWrite when the memory map is rewritten)
- A stick whose disk number changes during the write is found again instead of stopping the build; a volume that
  disappears is skipped
- When Apple's download server resets the connection, the stop message says what to try on the network
- When Windows cannot format the stick, its own error is now in the log before the fallback

## 2.0.2

1401 for Windows
- A PC with no network adapter that works in the macOS installer gets its stick written, with a note to connect a
  supported Ethernet or Wi-Fi adapter before installing
- A stick that Windows gives a new disk number just before the erase is found again and written

No changes to the driver or the Mac app since 2.0.0.

## 2.0.1

1401 for Windows
- PCs with an NVIDIA card can build for macOS 26 Tahoe. 2.0.0 offered only macOS 15 for them, so picking
  Tahoe stopped the build

No changes to the driver or the Mac app since 2.0.0.

## 2.0.0

macOS 15 Sequoia and macOS 26 Tahoe.

Security
- Fixes for every finding in the security report by @Mahasvan (#214). Thank you for reporting them to us first.
- The driver's administrator check now reads the caller's real user id; it used to answer yes for everyone
- Copies between NVKMS and a process can no longer reach kernel memory, and the NVKMS entry point is root-only
- First-time GPU bring-up requires root
- A process cannot map memory that belongs to another process
- Unknown client types are refused, the bind call checks its range and always drops a stale mapping, waits on GPU
  events are capped, and one process can no longer fill the client table (slots are kept for the system)
- The kernel-address check refuses to arm on any macOS build it was not made for
- The Mac app verifies a signature on its own updates before unpacking them, runs with the hardened runtime, and
  refuses to run its root scripts from a folder other users can write to

Driver
- GeForce RTX 50 series: compute kernels compiled by NVIDIA's own compiler now run on the card
- Fix the black screen with only a cursor after updating to 1.15 (video memory reserved over the boot screen was handed
  out again), including displays larger than the boot screen's reserved memory, such as 3840x2560 on cards with a
  256 MB memory window
- Fix flicker caused by a reused buffer showing an old frame
- A second monitor keeps its high refresh rates, and panels faster than 240 Hz are paced correctly
- Ultrawide, 4K and HiDPI modes are no longer capped at twice the boot resolution
- Cards with a small memory window: the display's own share is counted, so the desktop stops running out of space;
  a Turing card left at 64 MB is resized to the largest size it offers
- A display connected after startup comes up without a restart (#142, thanks @lsCoding666)
- macOS learns the panel's physical size, so scaling is right instead of assuming 72 dpi (#199, thanks @GeekAudio)
- Laptops with the screen on the Intel GPU: the driver no longer takes the Intel GPU's display slot (#131, #119, thanks
  @TararaDotJoshua), and muxless Turing GPUs get the startup settle they need
- Tahoe: ship the macOS 26 accelerator built from this release (1.15 carried an older one: black screen on Tahoe)
- Fix crashes in Photos and media analysis, in apps that split a command buffer at an event, in Chess, and in apps
  that free Vulkan memory (thanks @Exop63)
- Fix memory running out during Super Resolution and other long machine-learning work
- A texture copied for display stays in step on every submission (thanks @KolliasG7); work is reported as scheduled
  only after IOSurface writes finish (#147, thanks @imyago9); a surface keeps its video memory across page-off (#200,
  thanks @imbrahiam)
- World of Warcraft Classic and other Metal games: cube-map arrays, bounds-checked buffer reads and several shader fixes
  (#171, thanks @razvanccr); Firefox WebRender shaders translate (thanks @advokat11); MSAA sample positions on cards
  without programmable 1x positions (#136, thanks @eisda)
- Faster pipeline builds: built in parallel, one hash per shader module, planner results reused, and one private image
  per IOSurface (#173, #177, #181, #188, #189, thanks @modesttao)
- Chrome, Edge and Electron apps draw on the CPU again by default, which ends the artifacts, stutter and voice-call
  crashes in Discord on several cards; `NVMTL_ANGLE_METAL=1` turns their GPU path on
- Optional asynchronous texture uploads, off by default (`NVMTL_ASYNC_TEX_UPLOAD=1`)
- Multi-GPU display selection workaround documented in docs/MULTI-GPU.md (#197, thanks @is-pan)

Experimental, off unless you turn it on
- AMD and Intel GPU support (NMXGPU) and the NPU driver, for macOS 15 and 26: Setup > "AMD / Intel GPU driver (test)"

Mac app
- Finish a macOS update the update helper never finished, and keep this Mac's own BAR settings afterwards
- "Remove NVIDIA driver" stops with an explanation, instead of failing half-way, when System Integrity Protection is on
- The Remove NVIDIA driver startup entry is always visible, and removing the driver keeps the screen on
- Warn when the Mac starts OpenCore only from the USB stick and the internal disk has none
- Explain a failed config backup on a read-only OpenCore partition, and say which step comes first when Install is
  greyed out
- Send logs copies files before running log queries and includes crash reports under any name

1401 for Windows
- Pick macOS 15 Sequoia or macOS 26 Tahoe before the build; see the 1401 changelog for the full list

Known issues
- Preview not showing pictures and lost window transparency on some macOS 15 setups
- Chrome decodes video on the CPU while its GPU sandbox is on (#175)
- Thunderbolt eGPUs: no display
- Laptops with the screen on the Intel GPU: the built-in screen is not accelerated
- HDMI/DisplayPort audio: the display now reports its audio formats to macOS, but sound over HDMI/DisplayPort is not
  working yet

## 1.15.0

The last release for macOS 15 Sequoia.

Driver
- Fix black screen with only a cursor on GTX 16 and other cards without Resizable BAR (regression in 1.14)
- Fix Unity 6 games hanging on the first frame
- Fix windows and web pages showing old content after a redraw
- Fix high WindowServer CPU and low frame rate at 4K after a few minutes
- Fix slow scrolling (window copies up to 35x faster)
- Fix monitors shown as a 30-inch TV in About This Mac
- Fix 165 Hz mode missing on some monitors (since 1.14.2)
- Retry a display that stays dark after sleep on the next wake
- Fix Chrome, Edge and Electron apps drawing everything on the CPU (no WebGL, slow scrolling); their GPU path is on
  by default again and `NVMTL_ANGLE_METAL=0` turns it off
- Allow Chrome, Edge and Electron apps to use the GPU with their sandbox on
- Fix 5120x1440 and other ultrawide modes missing when the firmware started at a smaller resolution
- Fix green lines in Photos and other VideoToolbox scaling (#179, #180, thanks @modesttao)
- Add T400, T600, T1000 and T-series laptop GPUs (#132, thanks @TararaDotJoshua)

Mac app
- Show download progress in Update driver, which looked stuck at "Downloading the newest driver..."
- Ask for access to USB sticks instead of failing with "no OpenCore config"
- Say when the boot stick is unplugged
- Recover from an interrupted install instead of stopping at "installer guard already exists"
- Download from the NullMoth mirror when GitHub is blocked
- Send logs: always finish, keep the start of each boot, stop re-sending old logs

1401 for Windows
- Fix the build stopping on ACPI tables that declare a method twice (BIOSTAR B650 and similar)

Other
- Grammar fixes in the release notes (#128, #129, thanks @lunar-me)
- Translator test fix (#172, thanks @modesttao)
- Release notes moved into this file; add CONTRIBUTING.md and a pull request template

Known issues
- If Chrome pages show corrupted areas on your card, start it with `NVMTL_ANGLE_METAL=0` and send logs
- Ghost or black areas after windows overlap with a BAR of 4 GB or more (#127)
- Chrome decodes video on the CPU while its GPU sandbox is on: VideoToolbox picks a decoder before the driver loads (#175)
- Thunderbolt eGPUs: no display
- Laptops with the screen on the Intel GPU: the built-in screen is not accelerated
- No HDMI/DisplayPort audio

## 1.14.7
- Stop driver updates from turning the EFI boot logo back on
- Keep the existing OpenCore BAR settings when installing
- Show install, cancel and failure in the updater after the download finishes

## 1.14.6
- Fix Firefox WebGL inside the GPU sandbox
- Turn off the Chromium ANGLE Metal path by default after corruption reports on RTX 30 and 50
- Retry Apple recovery downloads that return HTTP 403

## 1.14.5
- Fix Unity 6 games hanging on the first frame
- Keep Chromium WebGL on the GPU when the sandbox blocks a memory query
- Send logs: give each log query its own time limit

## 1.14.4
- Fix full-screen flicker on machines that copy frames to the screen
- Send logs includes the AMD/Intel test driver

## 1.14.3
- Stop macOS writing to firmware memory on Ryzen 7000/8000/9000 (AM5)

## 1.14.2
- Fix black login window on high refresh rate monitors
- Fix SecurityAgent and Installer crashes on some RTX 40 PCs
- Fix apps quitting on a GPU check (Safari, photo analysis)
- Fix the AMD/Intel test driver firmware download

## 1.14.1
- Fix PCs that stopped starting after 1.14.0
- Fix the boot logo and test driver switches doing nothing

## 1.14.0
- Fix black screen with a cursor about a minute after start-up (#16, #26)
- Fix 4K at 120 Hz and other DSC modes going black (#77)
- Turn the monitor off when the display sleeps (#90)
- Bring a display back after switching inputs (#110)
- Fix black screen after 100 seconds on older Intel boards without Resizable BAR

## 1.13.0
- Fix Turing cards stuck at start-up after updating
- Fix "Remove NVIDIA driver" not removing the driver, or hanging
- Crash reports no longer show crashes from before the update

## 1.12.0
- Use the NVIDIA GPU on laptops whose screen is on the Intel GPU (as a second Metal device)
- Record display state from the first second of each boot for flicker reports
- Fix Live Text crashing in the Vulkan driver

## 1.11.0
- Download Lilu and AMFIPass when they are missing from the EFI
- Fix Firefox crashing at launch (`ipc_control_port_options=0`)
- Fix "STOP back up nvmtl"
- Add USBToolBox when it is missing

## 1.10.0
- Fix AM5 Ryzen PCs stuck at `EB.MM.AKM`
- Drop the DMAR table on Intel Core Ultra
- Reach Apple's recovery server directly when DNS fails

## 1.9.0
- Try one more memory setting on AM5 boards
- Erase stubborn USB sticks with diskpart

## 1.8.0
- Use known-good settings for boards that have booted before
- Fix "Format-Volume: Invalid property" and "WinError 2" when erasing

## 1.7.0
- Pass the board's DSDT to macOS again (was dropped on about 1 in 5 PCs)
- Send logs delivers the flicker capture and OpenCore config

## 1.6.0
- Fix the driver removing itself after login on some boards
- Fix `RmInitAdapter failed` on Ryzen with a custom EFI
- Send logs includes the OpenCore config (serials removed)

## 1.5.0
- Finish the macOS download on networks that cut connections

## 1.4.0
- Clear flicker at login with a compositor restart
- Build on PCs whose ACPI tables crash the disassembler

## 1.3.0
- Fix Send logs freezing the app
- Fix the crash loop on 16 GB RTX 50 cards
- Light up a monitor plugged in after login

## 1.2.0
- Fix "Remove NVIDIA driver" so the driver stays off until removed
- Show the picture with macOS's display driver while ours is off

## 1.1.0
- Fix recycled allocations reaching queued GPU work
- Fail a rejected display request on its own instead of stopping the display

## 1.0.x
First public releases: installer, updater, removal and USB map fixes.
