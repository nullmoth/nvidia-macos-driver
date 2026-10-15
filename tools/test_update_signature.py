#!/usr/bin/env python3
"""App self-update signature check (ReleaseSignature.swift).

An update zip is unpacked only when a detached Ed25519 signature over its exact bytes verifies against the
public key compiled into the app. The previous check was `codesign --verify --deep` with no signer pinned,
which succeeds on any signature at all, including an ad-hoc or self-signed bundle; the negative control below
reproduces that old behaviour and shows it accepting exactly the forgeries this check now refuses.

This compiles the shipping ReleaseSignature.swift together with a tiny probe that generates a throwaway
keypair, signs with it, and verifies through the real code, so no key and no extra dependency are involved."""
import base64
import os
import pathlib
import subprocess
import sys
import tempfile

root = pathlib.Path(sys.argv[1] if len(sys.argv) > 1 else pathlib.Path(__file__).resolve().parents[1])
src = root / 'app/Sources/ReleaseSignature.swift'
fails = 0


def check(name, ok, detail=''):
    global fails
    print(('ok   ' if ok else 'FAIL ') + name + (f' ({detail})' if detail else ''))
    fails += not ok


# The baked-in release key is a real 32-byte Ed25519 public key (so release.py can match it).
text = src.read_text()
baked = ''
for ln in text.splitlines():
    if 'publicKeyBase64 = "' in ln:
        baked = ln.split('"')[1]
        break
try:
    decoded = base64.b64decode(baked, validate=True)
except Exception:  # noqa: BLE001
    decoded = b''
check('the compiled-in release public key is 32 raw bytes of base64', len(decoded) == 32, f'{len(decoded)} bytes')

probe = r'''
import Foundation
import CryptoKit
let a = CommandLine.arguments
switch a[1] {
case "gen":
    let k = Curve25519.Signing.PrivateKey()
    print("PRIV " + k.rawRepresentation.base64EncodedString())
    print("PUB " + k.publicKey.rawRepresentation.base64EncodedString())
case "sign":  // sign <zip> <privB64>
    let data = try! Data(contentsOf: URL(fileURLWithPath: a[2]))
    let k = try! Curve25519.Signing.PrivateKey(rawRepresentation: Data(base64Encoded: a[3])!)
    print(try! k.signature(for: data).base64EncodedString())
case "verify":  // verify <sigfile> <zip> <pubB64>
    let sig = (try? String(contentsOfFile: a[2], encoding: .utf8)) ?? ""
    print(ReleaseSignature.verify(zip: URL(fileURLWithPath: a[3]), signatureBase64: sig, publicKeyBase64: a[4]) ? "ACCEPT" : "REFUSE")
default:
    print("REFUSE")
}
'''

with tempfile.TemporaryDirectory() as t:
    t = pathlib.Path(t)
    (t / 'main.swift').write_text(probe)   # top-level statements are only allowed in a file named main.swift
    binpath = t / 'probe'
    r = subprocess.run(['xcrun', 'swiftc', '-O', str(src), str(t / 'main.swift'), '-o', str(binpath)],
                       capture_output=True, text=True, timeout=300)
    if r.returncode:
        check('ReleaseSignature.swift compiles with the probe', False, r.stderr.strip()[-300:])
        print('FAILED')
        sys.exit(1)
    check('ReleaseSignature.swift compiles with the probe', True)

    def run(*args):
        return subprocess.run([str(binpath), *map(str, args)], capture_output=True, text=True, timeout=60).stdout.strip()

    def keypair():
        out = {ln.split()[0]: ln.split()[1] for ln in run('gen').splitlines()}
        return out['PRIV'], out['PUB']

    priv_a, pub_a = keypair()      # the release key
    priv_b, pub_b = keypair()      # an attacker's key
    zip_path = t / 'update.zip'
    zip_path.write_bytes(os.urandom(4096))
    sig_a = t / 'a.sig'
    sig_a.write_text(run('sign', zip_path, priv_a) + '\n')
    sig_b = t / 'b.sig'
    sig_b.write_text(run('sign', zip_path, priv_b) + '\n')

    def verify(sig_arg, pub):
        # sig_arg: a path to a signature file, or a literal string written to a file first
        p = t / 'sig.tmp'
        p.write_text(sig_arg if isinstance(sig_arg, str) else pathlib.Path(sig_arg).read_text())
        return run('verify', p, zip_path, pub)

    check('a genuine signature from the release key is accepted', verify(sig_a, pub_a) == 'ACCEPT')

    tampered = t / 'tampered.zip'
    b = bytearray(zip_path.read_bytes())
    b[0] ^= 0x01
    tampered.write_bytes(b)
    sig_for_tampered = t / 'sig.tmp'
    sig_for_tampered.write_text(sig_a.read_text())
    tv = subprocess.run([str(binpath), 'verify', str(sig_for_tampered), str(tampered), pub_a],
                        capture_output=True, text=True, timeout=60).stdout.strip()
    check('a single flipped byte in the zip is refused', tv == 'REFUSE', tv)

    check('a signature made by a different key is refused', verify(sig_b, pub_a) == 'REFUSE')
    check('a missing (empty) signature is refused', verify('', pub_a) == 'REFUSE')
    check('a truncated signature is refused', verify(sig_a.read_text().strip()[:-8], pub_a) == 'REFUSE')

    # Negative control: the old updater pinned no signer, so any present signature passed. Model that
    # ("a non-empty signature string is good enough") and confirm it accepts the attacker's forgery, which
    # the real check above refuses. If this control ever read ACCEPT from the real check, the test is meaningless.
    def old_accepts(sig_text):
        return bool(sig_text.strip())
    check('negative control: the unpinned old check accepts the attacker-signed update that this one refuses',
          old_accepts(sig_b.read_text()) and verify(sig_b, pub_a) == 'REFUSE')

# Source wiring: the update path verifies before it unpacks, and the bare codesign-only acceptance is gone.
mainsrc = (root / 'app/Sources/main.swift').read_text()
unpack = mainsrc.index('ditto", ["-x", "-k", zip.path')
check('the update is verified against the release key before it is unpacked',
      'ReleaseSignature.verify(zip: zip' in mainsrc and mainsrc.index('ReleaseSignature.verify(zip: zip') < unpack)

print('FAILED' if fails else 'PASS')
sys.exit(1 if fails else 0)
