import Foundation
import Darwin

/// Redact complete identity tokens, never substrings inside hardware symbols.
func redactSupportText(_ source: String, homes: [String], identities: [String: String]) -> String {
    var text = source
    for home in homes.filter({ !$0.isEmpty }).sorted(by: { $0.count > $1.count }) {
        text = text.replacingOccurrences(of: NSRegularExpression.escapedPattern(for: home) + #"(?=/|\s|["']|$)"#,
                                         with: "[home]", options: [.regularExpression, .caseInsensitive])
    }
    for (identity, replacement) in identities.filter({ $0.key.count > 2 }).sorted(by: { $0.key.count > $1.key.count }) {
        let pattern = #"(?<![\p{L}\p{N}_.-])"# + NSRegularExpression.escapedPattern(for: identity) + #"(?![\p{L}\p{N}_.-])"#
        text = text.replacingOccurrences(of: pattern, with: replacement, options: [.regularExpression, .caseInsensitive])
    }
    // Validate colon-separated candidates so PCI IDs and technical symbols survive.
    let ipv6 = try! NSRegularExpression(pattern: #"(?<![A-Za-z0-9_:])([0-9A-Fa-f:.]*:[0-9A-Fa-f:.]*:[0-9A-Fa-f:.]*)(?:%[A-Za-z0-9_.-]+)?(?![A-Za-z0-9_:])"#)
    let ns = text as NSString
    for match in ipv6.matches(in: text, range: NSRange(location: 0, length: ns.length)).reversed() {
        let address = ns.substring(with: match.range(at: 1))
        var parsed = in6_addr()
        if address.withCString({ inet_pton(AF_INET6, $0, &parsed) }) == 1 {
            if let range = Range(match.range, in: text) { text.replaceSubrange(range, with: "[ip]") }
        }
    }
    let rules: [(String, String)] = [
        (#"(?i)[/]Users[/][^/\s"']+"#, "[home]"),
        (#"\b[0-9A-Fa-f]{8}-[0-9A-Fa-f]{4}-[0-9A-Fa-f]{4}-[0-9A-Fa-f]{4}-[0-9A-Fa-f]{12}\b"#, "[uuid]"),
        (#"\b(?:[0-9A-Fa-f]{2}[:-]){5}[0-9A-Fa-f]{2}\b"#, "[mac]"),
        (#"\b(?:\d{1,3}\.){3}\d{1,3}\b"#, "[ip]"),
        (#"(?i)"(crashReporterKey|deviceIdentifierForVendor|sessionID|userID|incident|bootSessionUUID|sleepWakeUUID|serial[A-Za-z]*)"\s*:\s*"[^"]*""#, "\"$1\":\"[removed]\""),
        (#"(?i)(serial number|system serial|hardware uuid|provisioning udid)[^\n]*"#, "$1: [removed]"),
    ]
    // A crash report's binary-image UUIDs name the build of each library or kext in its stack ("uuid" : "..." in
    // usedImages, ["...", base, "K"] in a stackshot), not the machine; without them no frame can be resolved (2.1.0
    // WindowServer watchdog reports arrived with every image as [uuid]). They are set aside before the UUID rule and
    // put back after it. Machine identities (crashReporterKey, bootSessionUUID, hardware UUID) are matched by key above.
    var kept: [String] = []
    let image = try! NSRegularExpression(pattern: #"("uuid"\s*:\s*"|\[")([0-9A-Fa-f]{8}-[0-9A-Fa-f]{4}-[0-9A-Fa-f]{4}-[0-9A-Fa-f]{4}-[0-9A-Fa-f]{12})(?=",)"#)
    let ins = text as NSString
    for match in image.matches(in: text, range: NSRange(location: 0, length: ins.length)).reversed() {
        guard let range = Range(match.range(at: 2), in: text) else { continue }
        kept.append(String(text[range]))
        text.replaceSubrange(range, with: "\u{1}IMG\(kept.count - 1)\u{1}")
    }
    for (pattern, replacement) in rules { text = text.replacingOccurrences(of: pattern, with: replacement, options: .regularExpression) }
    for (i, uuid) in kept.enumerated() { text = text.replacingOccurrences(of: "\u{1}IMG\(i)\u{1}", with: uuid) }
    return text
}

/// A fresh send-run identifier; it contains no account or machine identity.
func newUploadBatch() -> String {
    "1401-mac-" + (0..<8).map { _ in String(format: "%02x", UInt8.random(in: 0...255)) }.joined()
}
