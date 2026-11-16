# Building the fork's NVIDIA 620.70.06 kext

This rebuilds NVRM from the fork's OS interfaces and NVIDIA's open source RM/KMS
cores. It does not install anything, modify EFI, or switch to the upstream 2.9
kernel/renderer ABI or broader display policies. The narrow
[notebook TGP backport](NOTEBOOK-TGP.md) remains opt-in. Use macOS with Xcode Command Line Tools,
Python 3 and enough free space for an isolated build.

Fetch a clean checkout of NVIDIA's 620.70.06 revision, then use a new work folder:

```sh
git clone --branch 620.70.06 --depth 1 https://github.com/NVIDIA/open-gpu-kernel-modules.git vendor-nvidia-620
git -C vendor-nvidia-620 rev-parse HEAD
# Must be e6a2e01579fbbd1ca2ed43fef2ee2589e9bd8bd9
mkdir -p out/nvrm-620-work
bash build/build_rm620.sh "$PWD/out/nvrm-620-work" "$PWD/vendor-nvidia-620"
bash build/build_kms620.sh "$PWD/out/nvrm-620-work"
bash build/build_nvrm.sh "$PWD/kexts/NVRM" \
  "$PWD/out/nvrm-620-work/ogkm-620.70.06" "$PWD/out/nvrm-620-kext"
```

`JOBS=4` can limit the RM/KMS compiler processes. The RM step requires a clean,
pinned checkout and refuses to reuse an existing work tree. It applies only
this fork's five NVIDIA patches, without fuzz, and adds the Darwin platform
declarations needed by the public build. The source identity, patch hashes and
archive digests are recorded. The final kext build verifies those identities
before linking; it refuses an existing output folder.

The modeset command transform removes NVIDIA's Linux external-return-thunk
compiler option. XNU does not export that Linux symbol; leaving it in produces
a kext that links as a file but cannot link into a macOS kernel collection.
The build refuses a KMS archive that still imports it.

The resulting kext targets x86_64 macOS 15.0 and is shared by Sequoia and Tahoe.
The complete package still requires Sequoia 15.5+ or Tahoe. Keep its matching
620.70.06 firmware, NVK renderer, framebuffer and both OS-specific accelerators.
Do not mix in an older NVIDIA core or a different NVKMS interface.

For packaging, apply the intended release version and dependency metadata,
sign the kext and verify its architecture, imports and kernel-collection link
in isolation. The output alone is not a boot qualification. Keep build records
outside `pkgroot/`; the runtime firmware and NVIDIA license still need to come
from the verified package inputs.
