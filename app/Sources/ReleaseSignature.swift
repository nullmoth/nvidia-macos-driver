// 1401
// Copyright (c) 2026 NullMoth Systems. SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0

import Foundation
import CryptoKit

// App self-update authenticity. codesign --verify only proves a bundle is internally consistent with whatever
// signature it carries, and it succeeds on an ad-hoc or self-signed bundle, so it cannot answer "who signed this".
// The release key's public half is compiled in below; its private half signs every 1401-Mac-*.zip at release time
// and never leaves the signing host. An update is unpacked only when a detached Ed25519 signature over the exact
// downloaded zip bytes verifies against this key, so an attacker who controls the download or the release page
// cannot get a tampered or self-signed app past the check.
enum ReleaseSignature {
    // Ed25519 public key, raw 32 bytes, base64. The private key lives off the build host (release.py signs with it).
    static let publicKeyBase64 = "ztPmcj+mcHYM1X2lrp24yjq0xBuzke7gh5XJ3E2fd4E="

    // Verify a detached signature (raw 64-byte Ed25519, base64) over the exact bytes of `zip` against the
    // compiled-in release key. Missing/empty/truncated signature, wrong key, or altered bytes all return false.
    static func verify(zip: URL, signatureBase64: String) -> Bool {
        verify(zip: zip, signatureBase64: signatureBase64, publicKeyBase64: publicKeyBase64)
    }

    // Same, with the public key passed in. The app always uses the compiled-in key; the extra argument lets the
    // test suite exercise this exact code against a throwaway keypair without touching the real release key.
    static func verify(zip: URL, signatureBase64: String, publicKeyBase64: String) -> Bool {
        let trimmed = signatureBase64.trimmingCharacters(in: .whitespacesAndNewlines)
        guard let sig = Data(base64Encoded: trimmed), sig.count == 64,
              let keyData = Data(base64Encoded: publicKeyBase64),
              let key = try? Curve25519.Signing.PublicKey(rawRepresentation: keyData),
              let data = try? Data(contentsOf: zip) else { return false }
        return key.isValidSignature(sig, for: data)
    }
}
