#!/usr/bin/env python3
"""The package carries the licence and notice of everything in it, and installs them beside the driver.

The package shipped NVIDIA's GSP firmware, the LGPL shader translator and the driver itself with none of their licence
texts. The release tooling now copies the files below into the package's licenses/ folder and refuses a release when
one is missing; install.sh copies that folder to /Library/NullMoth/licenses and uninstall.sh removes it."""
import pathlib
import re
import sys

root = pathlib.Path(__file__).resolve().parents[1]
fails = 0


def check(name, ok, detail=''):
    global fails
    print(('ok   ' if ok else 'FAIL ') + name + (f' ({detail})' if detail else ''))
    fails += not ok


texts = ['LICENSE', 'NOTICE', 'translator/LICENSE', 'translator/COPYING', 'licenses/NVIDIA-firmware-LICENCE.txt',
         'licenses/NVIDIA-open-gpu-kernel-modules-COPYING.txt', 'licenses/Mesa-license.rst',
         'vendor/nvenc/LICENSE', 'vendor/nvenc/THIRD_PARTY_NOTICES.md',
         'vendor/nvenc/vendor/open-gpu-kernel-modules/COPYING',
         'vendor/nvenc/vendor/open-gpu-doc/LICENSE.md']
missing = [t for t in texts if not (root / t).is_file() or not (root / t).stat().st_size]
check('every licence text the package ships is in the repository', not missing, ', '.join(missing))
firmware = (root / 'licenses/NVIDIA-firmware-LICENCE.txt').read_text(errors='replace') if not missing else ''
check('the firmware licence is NVIDIA\'s', 'License For Customer Use of NVIDIA Software' in firmware)
notice = (root / 'NOTICE').read_text()
check('NOTICE names the NVIDIA release the driver is built on, not an older one',
      '615.78.08' in notice and 'r610' not in notice)
check('NOTICE points at each licence text', all(f in notice for f in ('NVIDIA-firmware-LICENCE.txt',
      'NVIDIA-open-gpu-kernel-modules-COPYING.txt', 'Mesa-license.rst', 'translator/LICENSE')))
check('NOTICE keeps NVEncVT and its third-party terms separate from the repository licence',
      all(f in notice for f in ('mickys/nvenc v0.9.5', 'NVEncVT-MIT-LICENSE.txt',
          'NVEncVT-THIRD_PARTY_NOTICES.md', 'NVEncVT-NVIDIA-open-gpu-kernel-modules-COPYING.txt',
          'NVEncVT-open-gpu-doc-LICENSE.md', 'BSD-3-Clause')))
nvenc_notice = (root / 'vendor/nvenc/THIRD_PARTY_NOTICES.md').read_text(errors='replace') if not missing else ''
check('NVEncVT notices preserve the exact pinned NVIDIA, open-gpu-doc, and Apple license terms',
      all(s in nvenc_notice for s in ('610.57.04', 'MIT unless noted', 'GPL-2.0',
          'Copyright (C) 2020 Apple Inc.',
          'THIS SOFTWARE IS PROVIDED BY APPLE INC.')))
open_gpu_doc_license = (root / 'vendor/nvenc/vendor/open-gpu-doc/LICENSE.md').read_text(errors='replace') if not missing else ''
check('the open-gpu-doc MIT copyright and permission are retained',
      'Copyright (c) 2019, NVIDIA CORPORATION' in open_gpu_doc_license
      and 'Permission is hereby granted' in open_gpu_doc_license)
install = (root / 'package/install.sh').read_text()
check('install.sh installs the licences beside the driver',
      'ditto "$HERE/licenses" /Library/NullMoth/licenses' in install)
uninstall = (root / 'package/uninstall.sh').read_text()
check('uninstall.sh removes them with the driver', re.search(r'rm -rf [^\n]*/Library/NullMoth/licenses', uninstall) is not None)
stager = (root / 'package/stage_nvencvt.py').read_text()
check('NVEncVT staging packages upstream MIT and third-party notices beside the library and in licenses/',
      all(s in stager for s in ('vendor/nvenc/LICENSE', 'vendor/nvenc/THIRD_PARTY_NOTICES.md',
          'NVEncVT-MIT-LICENSE.txt', 'NVEncVT-THIRD_PARTY_NOTICES.md',
          'NVEncVT-NVIDIA-open-gpu-kernel-modules-COPYING.txt', 'NVEncVT-open-gpu-doc-LICENSE.md')))
print(f'{fails} failed')
sys.exit(1 if fails else 0)
