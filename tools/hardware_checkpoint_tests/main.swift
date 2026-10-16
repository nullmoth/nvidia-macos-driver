// Copyright (c) 2026 NullMoth Systems.
import Foundation
import Darwin
let folder = URL(fileURLWithPath: CommandLine.arguments[1])
var checks = 0
func check(_ value: @autoclosure () -> Bool, _ label: String) {
    checks += 1
    guard value() else { fatalError(label) }
}
for name in ["final-then-failure", "final-then-timeout", "final-then-signal", "normal", "legacy", "timeout", "signal", "failure", "malformed", "truncated", "truncated-success", "overflow", "small-cap", "changed-baseline", "missing-baseline-field", "wrong-phase", "overnodes-final", "missing-final", "emitter-cap", "third-record", "empty-extra", "invalid-baseline", "unavailable-overnodes", "missing"] {
    let start = ProcessInfo.processInfo.systemUptime
    let map = HardwareMapWorker.run(executable: folder.appendingPathComponent(name), timeout: name.contains("timeout") ? 1 : 2,
                                    maximumBytes: name == "small-cap" ? 512 : HardwareMapWorker.outputLimit)
    check(ProcessInfo.processInfo.systemUptime - start < 3, "bounded process duration")
    check(map["collector_checkpoint"] == nil, "protocol tag not exported")
    if ["invalid-baseline", "unavailable-overnodes", "missing"].contains(name) {
        check(map["status"] as? String == "unavailable", "invalid baseline refused")
    } else {
        check((map["nodes"] as? [[String: Any]])?.first?["id"] as? String == "n0", "completed nodes preserved (\(name))")
        check((map["cpu"] as? [String: Any])?["cores"] as? Int == 8, "completed CPU preserved")
        if name == "normal" || name == "legacy" { check(map["status"] as? String == "observed", "successful map selected") }
        else {
            check(map["status"] as? String == "partial", "failed optional output marked partial")
            check((map["peripheral_facts"] as? [String: Any])?["status"] as? String == "unavailable", "failed optional facts refused")
            check(map["collector_outcome"] is String, "explicit incomplete outcome")
        }
        if name == "normal" { check((map["peripheral_facts"] as? [String: Any])?["status"] as? String == "observed", "normal final selected") }
    }
    if name.contains("timeout") || name.contains("signal") {
        let pid = Int32(try! String(contentsOf: folder.appendingPathComponent(name + ".pid"), encoding: .utf8))!
        errno = 0; check(kill(pid, 0) == -1 && errno == ESRCH, "owned child killed and reaped")
    }
}
var bytes = 0
check(!HardwareMapWorker.emitCheckpoint(["schema": "nullmoth-hardware-map/1", "status": "unavailable"], phase: "baseline", bytesWritten: &bytes), "outside-child emission refused")
check(bytes == 0, "outside-child write budget unchanged")
print("PASS: \(checks) checkpoint assertions including real timeout/signal reaping, framing, caps and baseline preservation")
