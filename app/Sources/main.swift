// 1401
// Copyright (c) 2026 NullMoth Systems. SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0

import Cocoa
import CryptoKit
import IOKit
import Metal
import WebKit

struct Package {
    static let version = "1.0.8"
    static let name = "nullmoth-nvidia-1.0.8.tar.gz"
    static let url = URL(string: "https://github.com/nullmoth/nvidia-macos-driver/releases/download/v1.0.12/nullmoth-nvidia-1.0.8.tar.gz")!
    static let sha256 = "459e874e889f5913f4c88956c09fd827f1a2bb2c4f39ab1bbcb8af9094366ba2"
}
let uploadPage = URL(string: "https://nullmothsystems.com/#send")!
let support = FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask)[0].appendingPathComponent("NullMoth")
let logs = FileManager.default.urls(for: .libraryDirectory, in: .userDomainMask)[0].appendingPathComponent("Logs/NullMoth")

func sh(_ path: String, _ args: [String]) -> String {
    let p = Process(); p.executableURL = URL(fileURLWithPath: path); p.arguments = args
    let o = Pipe(); p.standardOutput = o; p.standardError = Pipe()
    do { try p.run() } catch { return "" }
    let d = o.fileHandleForReading.readDataToEndOfFile(); p.waitUntilExit()
    return String(decoding: d, as: UTF8.self)
}
func sysctl(_ k: String) -> String { sh("/usr/sbin/sysctl", ["-n", k]).trimmingCharacters(in: .whitespacesAndNewlines) }
func sha256(_ url: URL) -> String? {
    guard let h = try? FileHandle(forReadingFrom: url) else { return nil }
    defer { try? h.close() }
    var hasher = SHA256()
    while let d = try? h.read(upToCount: 1 << 20), !d.isEmpty { hasher.update(data: d) }
    return hasher.finalize().map { String(format: "%02x", $0) }.joined()
}
func json(_ o: Any) -> String {
    (try? JSONSerialization.data(withJSONObject: o, options: [.prettyPrinted, .sortedKeys])).map { String(decoding: $0, as: UTF8.self) } ?? "{}"
}

func prop(_ e: io_registry_entry_t, _ k: String) -> Any? {
    IORegistryEntryCreateCFProperty(e, k as CFString, kCFAllocatorDefault, 0)?.takeRetainedValue()
}
func u32(_ e: io_registry_entry_t, _ k: String) -> UInt32? {
    guard let d = prop(e, k) as? Data, d.count >= 4 else { return (prop(e, k) as? NSNumber)?.uint32Value }
    return d.withUnsafeBytes { $0.load(as: UInt32.self) }
}
func str(_ e: io_registry_entry_t, _ k: String) -> String? {
    if let s = prop(e, k) as? String { return s }
    if let d = prop(e, k) as? Data { return String(decoding: d.prefix { $0 != 0 }, as: UTF8.self) }
    return nil
}
func regName(_ e: io_registry_entry_t) -> String {
    var n = [CChar](repeating: 0, count: 128); IORegistryEntryGetName(e, &n); return String(cString: n)
}
func className(_ e: io_registry_entry_t) -> String {
    var n = [CChar](repeating: 0, count: 128); IOObjectGetClass(e, &n); return String(cString: n)
}
func children(_ e: io_registry_entry_t) -> [io_registry_entry_t] {
    var it: io_iterator_t = 0, out: [io_registry_entry_t] = []
    guard IORegistryEntryGetChildIterator(e, kIOServicePlane, &it) == KERN_SUCCESS else { return out }
    var c = IOIteratorNext(it); while c != 0 { out.append(c); c = IOIteratorNext(it) }
    IOObjectRelease(it); return out
}
func services(_ cls: String) -> [io_registry_entry_t] {
    var it: io_iterator_t = 0, out: [io_registry_entry_t] = []
    guard IOServiceGetMatchingServices(kIOMainPortDefault, IOServiceMatching(cls), &it) == KERN_SUCCESS else { return out }
    var s = IOIteratorNext(it); while s != 0 { out.append(s); s = IOIteratorNext(it) }
    IOObjectRelease(it); return out
}

func pciDisplays() -> [[String: Any]] {
    services("IOPCIDevice").compactMap { s in
        defer { IOObjectRelease(s) }
        guard let cls = u32(s, "class-code"), cls >> 16 == 0x03, let v = u32(s, "vendor-id"), let d = u32(s, "device-id") else { return nil }
        return ["vendor": String(format: "%04X", v & 0xffff), "device": String(format: "%04X", d & 0xffff), "model": str(s, "model") ?? ""]
    }
}

func usbControllers() -> [[String: Any]] {
    var out: [[String: Any]] = []
    for x in services("AppleUSBXHCI") {
        var parent: io_registry_entry_t = 0
        IORegistryEntryGetParentEntry(x, kIOServicePlane, &parent)
        let ven = u32(parent, "vendor-id").map { $0 & 0xffff } ?? 0, dev = u32(parent, "device-id").map { $0 & 0xffff } ?? 0
        var path = [CChar](repeating: 0, count: 512)
        IORegistryEntryGetPath(parent, kIOServicePlane, &path)
        var ports: [[String: Any]] = []
        for p in children(x) {
            let cls = className(p)
            if cls.hasPrefix("AppleUSB"), cls.hasSuffix("XHCIPort") {
                let devs = children(p).filter { className($0).contains("USB") && className($0).contains("Device") || className($0) == "IOUSBHostDevice" }
                let names = devs.map { str($0, "USB Product Name") ?? str($0, "kUSBProductString") ?? regName($0) }
                ports.append(["name": regName(p), "port": Int(u32(p, "port") ?? 0), "usb3": cls.contains("30"),
                              "connector": Int(u32(p, "UsbConnector") ?? 255), "devices": names,
                              "comment": str(p, "#comment") ?? ""])
                devs.forEach { IOObjectRelease($0) }
            }
            IOObjectRelease(p)
        }
        out.append(["controller": regName(parent), "vendor": String(format: "%04X", ven), "device": String(format: "%04X", dev),
                    "path": String(cString: path), "ports": ports.sorted { ($0["port"] as! Int) < ($1["port"] as! Int) }])
        IOObjectRelease(parent); IOObjectRelease(x)
    }
    return out
}

func utbMap(_ ctrls: [[String: Any]], _ sel: [String: [String: Int]]) -> ([String: Any]?, String?) {
    var pers: [String: Any] = [:]
    var ids = Set<String>()
    for c in ctrls {
        let name = c["controller"] as! String
        guard let chosen = sel[name], !chosen.isEmpty else { continue }
        if chosen.count > 15 { return (nil, "\(name) has \(chosen.count) ports picked; macOS allows 15 per controller. Untick some.") }
        let id = "0x\(c["device"]!)\(c["vendor"]!)"
        if ids.contains(id) { return (nil, "Two USB controllers share the PCI id \(id); this mapper cannot tell them apart yet.") }
        ids.insert(id)
        var ports: [String: Any] = [:]
        for p in c["ports"] as! [[String: Any]] {
            let pn = p["name"] as! String
            guard let conn = chosen[pn] else { continue }
            var n = UInt32(p["port"] as! Int).littleEndian
            ports[pn] = ["port": Data(bytes: &n, count: 4), "UsbConnector": conn,
                         "#comment": ((p["devices"] as? [String]) ?? []).joined(separator: ", ")]
        }
        pers[name] = ["CFBundleIdentifier": "com.dhinakg.USBToolBox.kext", "IOClass": "USBToolBox", "IOMatchCategory": "USBToolBox",
                      "IOPCIPrimaryMatch": id, "IOProviderClass": "IOPCIDevice",
                      "IOProviderMergeProperties": ["ports": ports, "port-count": Data(bytes: [UInt8(ports.count), 0, 0, 0], count: 4)]]
    }
    if pers.isEmpty { return (nil, "No ports picked.") }
    return (["CFBundleDevelopmentRegion": "English", "CFBundleIdentifier": "com.nullmoth.UTBMap", "CFBundleInfoDictionaryVersion": "6.0",
             "CFBundleName": "UTBMap", "CFBundlePackageType": "KEXT", "CFBundleShortVersionString": "1.0", "CFBundleSignature": "????",
             "CFBundleVersion": "1.0", "IOKitPersonalities": pers, "OSBundleLibraries": ["com.dhinakg.USBToolBox.kext": "1.0.0"],
             "OSBundleRequired": "Root"], nil)
}

let crashDirs = ["/Library/Logs/DiagnosticReports", NSHomeDirectory() + "/Library/Logs/DiagnosticReports"]
let seenFile = support.appendingPathComponent("crash-seen.json")

let driverImages = ["NVMTLDriver", "libnvmtl_translate", "libvulkan_nouveau", "NVIDIAShared"]
func involvesDriver(_ u: URL) -> Bool {
    guard let t = try? String(contentsOf: u, encoding: .utf8) else { return false }
    let body = t.split(separator: "\n", maxSplits: 1).last.map(String.init) ?? t
    if let j = try? JSONSerialization.jsonObject(with: Data(body.utf8)) as? [String: Any] {
        if let ps = j["panicString"] as? String { return panicNamesDriver(ps) }
        guard let fi = j["faultingThread"] as? Int, let th = j["threads"] as? [[String: Any]], fi < th.count,
              let fr = th[fi]["frames"] as? [[String: Any]], let imgs = j["usedImages"] as? [[String: Any]] else { return false }
        return fr.prefix(30).contains { f in
            let ii = f["imageIndex"] as? Int ?? -1
            let n = ii >= 0 && ii < imgs.count ? (imgs[ii]["name"] as? String ?? "") : ""
            return driverImages.contains { n.contains($0) }
        }
    }
    return panicNamesDriver(t)
}
func panicNamesDriver(_ p: String) -> Bool {
    guard let r = p.range(of: "Kernel Extensions in backtrace") else { return false }
    let tail = p[r.upperBound...].prefix(4000)
    let block = tail.components(separatedBy: "\n\n").first ?? String(tail)
    return block.contains("com.nullmoth.")
}

func driverCrashes(sinceInstallOnly: Bool = true) -> [URL] {
    let fm = FileManager.default
    let installed = (try? fm.attributesOfItem(atPath: "/Library/NullMoth/state"))?[.modificationDate] as? Date
    var out: [URL] = []
    for d in crashDirs {
        for n in (try? fm.contentsOfDirectory(atPath: d)) ?? [] where !n.hasPrefix(".") && (n.hasSuffix(".panic") || n.hasSuffix(".ips")) {
            let u = URL(fileURLWithPath: d).appendingPathComponent(n)
            let m = (try? fm.attributesOfItem(atPath: u.path))?[.modificationDate] as? Date ?? .distantPast
            if sinceInstallOnly, let installed, m < installed { continue }
            if involvesDriver(u) { out.append(u) }
        }
    }
    return out.sorted { $0.lastPathComponent < $1.lastPathComponent }
}

func redact(_ s: String) -> String {
    var t = s
    let user = NSUserName(), full = NSFullUserName()
    var exact: [String: String] = [:]
    exact[NSHomeDirectory()] = "[home]"; exact[URL(fileURLWithPath: "/Users").appendingPathComponent(user).path] = "[home]"
    for k in ["ComputerName", "LocalHostName", "HostName"] {
        let v = sh("/usr/sbin/scutil", ["--get", k]).trimmingCharacters(in: .whitespacesAndNewlines)
        if v.count > 2 { exact[v] = "this-mac" }
    }
    for e in services("IOPlatformExpertDevice") {
        for k in ["IOPlatformSerialNumber", "IOPlatformUUID", "serial-number"] { if let v = str(e, k), v.count > 3 { exact[v] = "[removed]" } }
        IOObjectRelease(e)
    }
    if full.count > 2 { exact[full] = "user" }
    if user.count > 2 { exact[user] = "user" }
    for (k, v) in exact.sorted(by: { $0.key.count > $1.key.count }) { t = t.replacingOccurrences(of: k, with: v) }
    let rules: [(String, String)] = [
        (#"[/]Users[/][^/\s"']+"#, "[home]"),
        (#"\b[0-9A-Fa-f]{8}-[0-9A-Fa-f]{4}-[0-9A-Fa-f]{4}-[0-9A-Fa-f]{4}-[0-9A-Fa-f]{12}\b"#, "[uuid]"),
        (#"\b(?:[0-9A-Fa-f]{2}[:-]){5}[0-9A-Fa-f]{2}\b"#, "[mac]"),
        (#"\b(?:\d{1,3}\.){3}\d{1,3}\b"#, "[ip]"),
        (#""(crashReporterKey|deviceIdentifierForVendor|sessionID|userID|incident|bootSessionUUID|sleepWakeUUID|serial[A-Za-z]*)"\s*:\s*"[^"]*""#, "\"$1\":\"[removed]\""),
        (#"(?i)(serial number|system serial|hardware uuid|provisioning udid)[^\n]*"#, "$1: [removed]"),
    ]
    for (p, r) in rules { t = t.replacingOccurrences(of: p, with: r, options: .regularExpression) }
    return t
}

func hardwareFacts() -> String {
    let os = ProcessInfo.processInfo.operatingSystemVersion
    let build = sh("/usr/bin/sw_vers", ["-buildVersion"]).trimmingCharacters(in: .whitespacesAndNewlines)
    let gpus = pciDisplays().map { "\($0["vendor"]!):\($0["device"]!) \($0["model"] as? String ?? "")" }.joined(separator: "; ")
    let mem = (UInt64(sysctl("hw.memsize")) ?? 0) >> 30
    let kexts = sh("/usr/bin/kmutil", ["showloaded", "--list-only"]).split(separator: "\n")
        .filter { $0.contains("com.nullmoth.") }.map { String($0.split(separator: " ").last(where: { $0.hasPrefix("com.") }) ?? "") }
    return """
    1401 crash report (made on this Mac; send it only if you want to)
    driver package : \(Package.version)
    macOS          : \(os.majorVersion).\(os.minorVersion).\(os.patchVersion) (\(build))
    CPU            : \(sysctl("machdep.cpu.brand_string")) (\(sysctl("hw.physicalcpu")) cores / \(sysctl("hw.logicalcpu")) threads)
    memory         : \(mem) GB
    Mac model      : \(sysctl("hw.model"))
    graphics       : \(gpus)
    driver kexts   : \(kexts.isEmpty ? "none loaded now" : kexts.joined(separator: ", "))
    OpenCore       : \(sh("/usr/sbin/nvram", ["4D1FDA02-38C7-4A6A-9CC6-4BCCA8B30102:opencore-version"]).split(separator: "\t").last.map { $0.trimmingCharacters(in: .whitespacesAndNewlines) } ?? "unknown")

    """
}

func failurePart(_ u: URL) -> String {
    guard let t = try? String(contentsOf: u, encoding: .utf8) else { return "" }
    var body = t
    if let j = t.split(separator: "\n", maxSplits: 1).last.flatMap({ try? JSONSerialization.jsonObject(with: Data($0.utf8)) as? [String: Any] }) {
        if let ps = j["panicString"] as? String { body = ps }
        else {
            var parts: [String] = []
            if let ex = j["exception"] { parts.append("exception: \(ex)") }
            if let te = j["termination"] { parts.append("termination: \(te)") }
            if let fi = j["faultingThread"] as? Int, let th = j["threads"] as? [[String: Any]], fi < th.count,
               let fr = th[fi]["frames"] as? [[String: Any]], let imgs = j["usedImages"] as? [[String: Any]] {
                parts.append("crashed thread \(fi):")
                for (i, f) in fr.prefix(40).enumerated() {
                    let ii = f["imageIndex"] as? Int ?? -1
                    let img = ii >= 0 && ii < imgs.count ? (imgs[ii]["name"] as? String ?? "?") : "?"
                    parts.append("  \(i) \(img) \(f["symbol"] as? String ?? "") +\(f["imageOffset"] ?? "")")
                }
            }
            body = parts.joined(separator: "\n")
        }
    }
    return String(body.prefix(120_000))
}

func makeReport(_ crashes: [URL]) -> String {
    var r = ""
    for u in crashes.suffix(3) {
        r += "\n=== \(u.lastPathComponent.replacingOccurrences(of: #"-\d{4}-\d{2}-\d{2}-\d{6}"#, with: "", options: .regularExpression)) ===\n"
        r += failurePart(u) + "\n"
    }
    return hardwareFacts() + redact(r)
}

func writeReport(_ crashes: [URL], to dst: URL) -> Bool {
    (try? makeReport(crashes).write(to: dst, atomically: true, encoding: .utf8)) != nil
}

func crashCheck(ui: Bool) -> Int32 {
    let seen = Set((try? JSONSerialization.jsonObject(with: Data(contentsOf: seenFile))) as? [String] ?? [])
    let new = driverCrashes().filter { !seen.contains($0.lastPathComponent) }
    print(json(["new": new.map(\.lastPathComponent)]))
    guard !new.isEmpty else { return 0 }
    try? FileManager.default.createDirectory(at: support, withIntermediateDirectories: true)
    if let d = try? JSONSerialization.data(withJSONObject: Array(seen) + new.map(\.lastPathComponent)) { try? d.write(to: seenFile, options: .atomic) }
    guard ui else { return 0 }
    let app = NSApplication.shared; app.setActivationPolicy(.accessory); app.activate(ignoringOtherApps: true)
    let a = NSAlert()
    a.messageText = "1401 noticed the NVIDIA driver crashed your system."
    a.informativeText = "Would you like to send a report? 1401 makes a text file on your Desktop with your hardware and what failed. It leaves out your name, your Mac's name, serial numbers and addresses. You can read it first, then upload it on the NullMoth site. Nothing is sent unless you upload it."
    a.addButton(withTitle: "Make the report"); a.addButton(withTitle: "Not now")
    if let img = Bundle.main.image(forResource: "moth-mark") { a.icon = img }
    if a.runModal() == .alertFirstButtonReturn {
        let f = DateFormatter(); f.dateFormat = "yyyy-MM-dd-HHmm"
        let dst = FileManager.default.urls(for: .desktopDirectory, in: .userDomainMask)[0].appendingPathComponent("1401-crash-\(f.string(from: Date())).txt")
        if writeReport(new, to: dst) {
            NSWorkspace.shared.activateFileViewerSelecting([dst])
            NSWorkspace.shared.open(uploadPage)
        }
    }
    return 0
}

final class App: NSObject, NSApplicationDelegate, WKScriptMessageHandler, WKUIDelegate, URLSessionDownloadDelegate {
    var window: NSWindow!
    var web: WKWebView!
    var table: [String: Any] = [:]
    var seenUsb: [String: Set<String>] = [:]
    var usbTimer: Timer?

    func applicationDidFinishLaunching(_ n: Notification) {
        let res = Bundle.main.resourceURL!
        if let d = try? Data(contentsOf: res.appendingPathComponent("nvidia_gsp_ids.json")),
           let j = try? JSONSerialization.jsonObject(with: d) as? [String: Any] { table = j }
        let cfg = WKWebViewConfiguration()
        cfg.userContentController.add(self, name: "nm")
        web = WKWebView(frame: .zero, configuration: cfg)
        web.uiDelegate = self
        web.setValue(false, forKey: "drawsBackground")
        window = NSWindow(contentRect: NSRect(x: 0, y: 0, width: 880, height: 800),
                          styleMask: [.titled, .closable, .miniaturizable, .resizable], backing: .buffered, defer: false)
        window.title = "1401"; window.backgroundColor = .black; window.contentView = web; window.center()
        window.makeKeyAndOrderFront(nil)
        web.loadFileURL(res.appendingPathComponent("index.html"), allowingReadAccessTo: res)
        NSApp.activate(ignoringOtherApps: true)
    }
    func applicationShouldTerminateAfterLastWindowClosed(_ s: NSApplication) -> Bool { true }

    func webView(_ w: WKWebView, runJavaScriptConfirmPanelWithMessage m: String, initiatedByFrame f: WKFrameInfo, completionHandler: @escaping (Bool) -> Void) {
        let a = NSAlert(); a.messageText = m; a.addButton(withTitle: "OK"); a.addButton(withTitle: "Cancel")
        completionHandler(a.runModal() == .alertFirstButtonReturn)
    }

    func send(_ event: String, _ data: Any) {
        guard let j = try? JSONSerialization.data(withJSONObject: ["event": event, "data": data]),
              let s = String(data: j, encoding: .utf8) else { return }
        DispatchQueue.main.async { self.web.evaluateJavaScript("NM.on(\(s))") }
    }

    func userContentController(_ c: WKUserContentController, didReceive m: WKScriptMessage) {
        guard let b = m.body as? [String: Any], let act = b["act"] as? String else { return }
        switch act {
        case "scan": DispatchQueue.global().async { self.send("scan", self.scan()) }
        case "download": download()
        case "checkUpdate": checkUpdate()
        case "updateDriver": updateDriver(efi: b["efi"] as? String ?? "auto")
        case "run": run(mode: b["mode"] as? String ?? "dry", pkg: b["pkg"] as? String ?? "", efi: b["efi"] as? String ?? "auto", extra: [])
        case "restart": NSAppleScript(source: "tell application \"System Events\" to restart")?.executeAndReturnError(nil)
        case "privacy": NSWorkspace.shared.open(URL(string: "x-apple.systempreferences:com.apple.preference.security?Security")!)
        case "logs": try? FileManager.default.createDirectory(at: logs, withIntermediateDirectories: true); NSWorkspace.shared.open(logs)
        case "open": if let u = b["url"] as? String, let url = URL(string: u), url.scheme == "https" { NSWorkspace.shared.open(url) }
        case "usbStart": usbWatch(true)
        case "usbStop": usbWatch(false)
        case "usbWrite": usbWrite(b["sel"] as? [String: [String: Int]] ?? [:], efi: b["efi"] as? String ?? "auto")
        case "crashReport": crashReportFromWindow()
        case "sendLogs": sendLogs()
        case "osupdate":
            if b["cancel"] as? Bool ?? false { run(mode: "update", pkg: "", efi: b["efi"] as? String ?? "auto", extra: ["--update", "cancel"]) }
            else { run(mode: "tahoe", pkg: b["pkg"] as? String ?? "", efi: b["efi"] as? String ?? "auto", extra: ["--update", "prepare"]) }   // installs or updates the driver, then prepares
        case "swupdate": NSWorkspace.shared.open(URL(string: "x-apple.systempreferences:com.apple.Software-Update-Settings.extension")!)
        case "verbose": run(mode: "verbose", pkg: "", efi: b["efi"] as? String ?? "auto", extra: ["--verbose", (b["on"] as? Bool ?? false) ? "on" : "off"])
        default: break
        }
    }

    func scan() -> [String: Any] { App.scanMac(table) }
    static func scanMac(_ table: [String: Any]) -> [String: Any] {
        let ids = table["ids"] as? [String: String] ?? [:], tested = table["tested"] as? [String] ?? []
        let gpus: [[String: Any]] = pciDisplays().map { g in
            let nv = g["vendor"] as? String == "10DE", dev = g["device"] as? String ?? ""
            var r = g
            r["name"] = nv ? (ids[dev] ?? "NVIDIA \(dev)") : ((g["model"] as? String).flatMap { $0.isEmpty ? nil : $0 } ?? "\(g["vendor"]!):\(dev)")
            r["supported"] = nv && ids[dev] != nil; r["tested"] = nv && tested.contains(dev)
            return r
        }
        let os = ProcessInfo.processInfo.operatingSystemVersion
        let oc = sh("/usr/sbin/nvram", ["4D1FDA02-38C7-4A6A-9CC6-4BCCA8B30102:opencore-version"])
            .split(separator: "\t").dropFirst().first.map { $0.trimmingCharacters(in: .whitespacesAndNewlines) } ?? ""
        let kexts = sh("/usr/bin/kmutil", ["showloaded", "--list-only"]).split(separator: "\n").filter { $0.contains("com.nullmoth.") }.count
        let fm = FileManager.default
        let files = fm.fileExists(atPath: "/Library/GPUBundles/NVMTLDriver.bundle") && fm.fileExists(atPath: "/Library/Extensions/NVRM.kext")
        var arch = "x86_64"
        #if arch(arm64)
        arch = "arm64"
        #endif
        let safe = sh("/usr/sbin/nvram", ["boot-args"]).split(whereSeparator: { $0 == " " || $0 == "\t" || $0 == "\n" }).contains("-nvoff")
        return ["macos": "\(os.majorVersion).\(os.minorVersion).\(os.patchVersion)", "major": os.majorVersion, "arch": arch,
                "gpus": gpus, "opencore": oc, "kexts": kexts, "files": files, "metal": MTLCopyAllDevices().map { $0.name },
                "packages": findPackages(), "version": Package.version, "safemode": safe,
                "record": fm.fileExists(atPath: "/Library/NullMoth/state"), "crashes": driverCrashes().map(\.lastPathComponent),
                "translated": sysctl("sysctl.proc_translated") == "1",
                "profile": { var p = machineProfile(gpus); p["rules"] = Profile.select(rules(), p).ids; return p }()]
    }

    static func rules() -> [String: Any] {
        guard let d = try? Data(contentsOf: Bundle.main.resourceURL!.appendingPathComponent("nullmoth-rules.json")),
              let j = try? JSONSerialization.jsonObject(with: d) as? [String: Any] else { return [:] }
        return j
    }
    /// This Mac's profile for the per-system rules. The PC's real board/chipset come from the 1401 stick when it is
    /// plugged in (1401 for Windows writes NullMoth/system-profile.json; macOS only sees the SMBIOS model 1401 set).
    static func machineProfile(_ gpus: [[String: Any]]) -> [String: Any] {
        var p: [String: Any] = [:]
        if let nv = gpus.first(where: { $0["vendor"] as? String == "10DE" }), let dev = nv["device"] as? String {
            p["gpu_id"] = dev.uppercased(); p["arch"] = Profile.arch(dev)
        }
        let vendor = sysctl("machdep.cpu.vendor")
        p["cpu_vendor"] = vendor.contains("AMD") ? "amd" : vendor.contains("Intel") ? "intel" : vendor
        p["cpu"] = sysctl("machdep.cpu.brand_string")
        let bat = IOServiceGetMatchingService(kIOMainPortDefault, IOServiceMatching("AppleSmartBattery"))
        p["laptop"] = bat != 0; if bat != 0 { IOObjectRelease(bat) }
        p["egpu"] = Profile.nvidiaBehindThunderbolt()
        p["macos_major"] = ProcessInfo.processInfo.operatingSystemVersion.majorVersion
        for v in (try? FileManager.default.contentsOfDirectory(atPath: "/Volumes")) ?? [] {
            if let d = try? Data(contentsOf: URL(fileURLWithPath: "/Volumes/\(v)/NullMoth/system-profile.json")),
               let j = try? JSONSerialization.jsonObject(with: d) as? [String: Any] {
                p["windows"] = j
                if let c = j["chipset"] as? String { p["chipset"] = c }
                break
            }
        }
        return p
    }

    static func findPackages() -> [[String: String]] {
        let fm = FileManager.default
        var c = ((try? fm.contentsOfDirectory(atPath: "/Volumes")) ?? []).map { URL(fileURLWithPath: "/Volumes/\($0)/NullMoth/\(Package.name)") }
        c.append(fm.urls(for: .downloadsDirectory, in: .userDomainMask)[0].appendingPathComponent(Package.name))
        c.append(support.appendingPathComponent(Package.name))
        return c.filter { fm.fileExists(atPath: $0.path) }.map { u in
            ["path": u.path, "where": u.path.hasPrefix("/Volumes/") ? "the 1401 stick or disk image" : u.path.contains("/Downloads/") ? "Downloads" : "an earlier download",
             "ok": sha256(u) == Package.sha256 ? "yes" : "no"]
        }
    }

    // "Update driver": the newest driver release, not the version this app was built with. The package is checked against
    // the SHA256SUMS.txt published in the same release and installed by the normal install path (backup, OpenCore checks).
    struct DownloadPlan {
        let name: String
        let sha: String
        let efi: String?
    }
    var pendingDownload: (id: Int, plan: DownloadPlan)? = nil
    var runningSetup = false
    lazy var downloadSession = URLSession(configuration: .default, delegate: self, delegateQueue: .main)
    var latest: (version: String, name: String, url: URL, sha: String)? = nil
    static func versionKey(_ v: String) -> [Int] { v.split(separator: ".").map { Int($0) ?? 0 } }
    static func newer(_ a: String, than b: String) -> Bool {
        let x = versionKey(a), y = versionKey(b)
        for i in 0..<max(x.count, y.count) { let p = i < x.count ? x[i] : 0, q = i < y.count ? y[i] : 0; if p != q { return p > q } }
        return false
    }
    func installedDriverVersion() -> String {
        (try? String(contentsOfFile: "/Library/NullMoth/driver-version", encoding: .utf8))?.trimmingCharacters(in: .whitespacesAndNewlines) ?? ""
    }
    func checkUpdate() {
        let api = URL(string: "https://api.github.com/repos/nullmoth/nvidia-macos-driver/releases/latest")!
        var rq = URLRequest(url: api); rq.setValue("application/vnd.github+json", forHTTPHeaderField: "Accept"); rq.timeoutInterval = 30
        URLSession.shared.dataTask(with: rq) { data, resp, err in
            let fail: (String) -> Void = { self.send("upd", ["state": "error", "why": $0]) }
            guard err == nil, (resp as? HTTPURLResponse)?.statusCode == 200, let data,
                  let j = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
                  let assets = j["assets"] as? [[String: Any]] else { return fail("could not reach the release list (\(err?.localizedDescription ?? "HTTP \((resp as? HTTPURLResponse)?.statusCode ?? 0)"))") }
            var pkg: (String, String, URL)? = nil; var sums: URL? = nil
            for a in assets {
                guard let n = a["name"] as? String, let u = (a["browser_download_url"] as? String).flatMap(URL.init(string:)), u.scheme == "https" else { continue }
                if n == "SHA256SUMS.txt" { sums = u }
                if let m = n.range(of: #"^nullmoth-nvidia-([0-9]+\.[0-9]+\.[0-9]+)\.tar\.gz$"#, options: .regularExpression) {
                    pkg = (String(n[m].dropFirst("nullmoth-nvidia-".count).dropLast(".tar.gz".count)), n, u)
                }
            }
            guard let pkg, let sums else { return fail("the latest release has no driver package or no SHA256SUMS.txt") }
            URLSession.shared.dataTask(with: sums) { sd, sr, se in
                guard se == nil, (sr as? HTTPURLResponse)?.statusCode == 200, let sd, let txt = String(data: sd, encoding: .utf8) else { return fail("could not read SHA256SUMS.txt") }
                let sha = txt.split(separator: "\n").compactMap { l -> String? in
                    let f = l.split(whereSeparator: { $0 == " " || $0 == "\t" }); return f.count >= 2 && f.last.map(String.init) == pkg.1 ? String(f[0]).lowercased() : nil }.first ?? ""
                guard sha.count == 64 else { return fail("SHA256SUMS.txt does not list \(pkg.1)") }
                DispatchQueue.main.async { self.latest = (pkg.0, pkg.1, pkg.2, sha) }
                let have = self.installedDriverVersion()
                self.send("upd", ["state": "checked", "latest": pkg.0, "installed": have,
                                  "newer": have.isEmpty || App.newer(pkg.0, than: have)])
            }.resume()
        }.resume()
    }
    func startDownload(name: String, sha: String, url: URL, efi: String?) {
        guard pendingDownload == nil, !runningSetup else { return }
        do { try FileManager.default.createDirectory(at: support, withIntermediateDirectories: true) }
        catch { send("dl", ["state": "error", "why": error.localizedDescription]); return }
        let task = downloadSession.downloadTask(with: url)
        pendingDownload = (task.taskIdentifier, DownloadPlan(name: name, sha: sha, efi: efi))
        send("dl", ["state": "start"])
        task.resume()
    }
    func updateDriver(efi: String) {
        guard let l = latest else { send("upd", ["state": "error", "why": "check for an update first"]); return }
        startDownload(name: l.name, sha: l.sha, url: l.url, efi: efi)
    }
    func download() {
        startDownload(name: Package.name, sha: Package.sha256, url: Package.url, efi: nil)
    }
    func urlSession(_ s: URLSession, downloadTask t: URLSessionDownloadTask, didWriteData b: Int64, totalBytesWritten w: Int64, totalBytesExpectedToWrite e: Int64) {
        guard pendingDownload?.id == t.taskIdentifier else { return }
        send("dl", ["state": "progress", "done": w, "total": e])
    }
    func urlSession(_ s: URLSession, downloadTask t: URLSessionDownloadTask, didFinishDownloadingTo loc: URL) {
        guard let pending = pendingDownload, pending.id == t.taskIdentifier else { return }
        let plan = pending.plan
        let code = (t.response as? HTTPURLResponse)?.statusCode ?? 0
        let dst = support.appendingPathComponent(plan.name)
        guard code == 200 else { send("dl", ["state": "error", "why": "the server answered HTTP \(code)"]); return }
        guard sha256(loc) == plan.sha else { send("dl", ["state": "error", "why": "the download does not match its SHA-256, so it was thrown away"]); return }
        do {
            if FileManager.default.fileExists(atPath: dst.path) {
                _ = try FileManager.default.replaceItemAt(dst, withItemAt: loc)
            } else { try FileManager.default.moveItem(at: loc, to: dst) }
        } catch { send("dl", ["state": "error", "why": error.localizedDescription]); return }
        pendingDownload = nil
        send("dl", ["state": "done", "path": dst.path])
        if let efi = plan.efi {
            run(mode: "install", pkg: dst.path, efi: efi, extra: [], expectedSha: plan.sha)
        }
    }
    func urlSession(_ s: URLSession, task: URLSessionTask, didCompleteWithError e: Error?) {
        guard pendingDownload?.id == task.taskIdentifier else { return }
        pendingDownload = nil
        if let e { send("dl", ["state": "error", "why": e.localizedDescription]) }
    }

    func usbWatch(_ on: Bool) {
        usbTimer?.invalidate(); usbTimer = nil
        if on {
            seenUsb = [:]
            usbTimer = Timer.scheduledTimer(withTimeInterval: 1.0, repeats: true) { _ in self.usbTick() }
            usbTick()
        }
    }
    func usbTick() {
        DispatchQueue.global().async {
            let c = usbControllers()
            for ctl in c {
                let n = ctl["controller"] as! String
                for p in ctl["ports"] as! [[String: Any]] where !((p["devices"] as? [String]) ?? []).isEmpty {
                    self.seenUsb[n, default: []].insert(p["name"] as! String)
                }
            }
            self.send("usb", ["controllers": c, "seen": self.seenUsb.mapValues { Array($0) }])
        }
    }
    func usbWrite(_ sel: [String: [String: Int]], efi: String) {
        let (plist, err) = utbMap(usbControllers(), sel)
        guard let plist else { send("usbErr", err ?? "could not build the map"); return }
        let dir = FileManager.default.temporaryDirectory.appendingPathComponent("nullmoth-utb-\(UUID().uuidString)/UTBMap.kext/Contents")
        try? FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        guard let d = try? PropertyListSerialization.data(fromPropertyList: plist, format: .xml, options: 0),
              (try? d.write(to: dir.appendingPathComponent("Info.plist"))) != nil else { send("usbErr", "could not write the map"); return }
        // the script wants the folder that HOLDS UTBMap.kext (10-07, NM-HZTKJHPZ: passing the .kext itself failed every map)
        run(mode: "usbmap", pkg: "", efi: efi, extra: ["--usbmap", dir.deletingLastPathComponent().deletingLastPathComponent().path])
    }

    // "Send logs" (two clicks: the button, then Send). Collects 1401's own logs, the driver's state, a redacted crash
    // report when the driver crashed, and - after macOS asks for the password - OpenCore's boot logs and saved panics
    // from every OpenCore partition (sticks included). Every file is uploaded with its SHA-256, and the site refuses
    // any upload whose bytes differ from it.
    func sendLogs() {
        let a = NSAlert()
        a.messageText = "Send logs to NullMoth"
        a.informativeText = "1401 is sending this Mac's NullMoth logs to nullmothsystems.com so the problem can be found and fixed: what 1401 did, the driver's state, driver crash reports, recent WindowServer crash reports, update diagnostics, and OpenCore's startup logs. Your name, your Mac's name, serial numbers and addresses are removed first. macOS asks for your password so 1401 can read the startup logs."
        a.addButton(withTitle: "Send"); a.addButton(withTitle: "Cancel")
        guard a.runModal() == .alertFirstButtonReturn else { send("logsDone", ["ok": false, "why": "Not sent."]); return }
        DispatchQueue.global().async {
            let fm = FileManager.default
            let dir = fm.temporaryDirectory.appendingPathComponent("1401-logs-\(Int(Date().timeIntervalSince1970))")
            try? fm.createDirectory(at: dir, withIntermediateDirectories: true)
            let script = Bundle.main.resourceURL!.appendingPathComponent("nullmoth-setup.sh").path
            let q = { (x: String) in "'" + x.replacingOccurrences(of: "'", with: "'\\''") + "'" }
            let cmd = "/bin/bash \(q(script)) --collect-logs \(q(dir.path)) > \(q(dir.appendingPathComponent("collect.txt").path)) 2>&1"
            var err: NSDictionary?
            NSAppleScript(source: "do shell script \"\(cmd.replacingOccurrences(of: "\\", with: "\\\\").replacingOccurrences(of: "\"", with: "\\\""))\" with administrator privileges")?.executeAndReturnError(&err)
            var files = ((try? fm.contentsOfDirectory(at: dir, includingPropertiesForKeys: nil)) ?? [])
            let mine = ((try? fm.contentsOfDirectory(at: logs, includingPropertiesForKeys: [.contentModificationDateKey])) ?? [])
                .sorted { ((try? $0.resourceValues(forKeys: [.contentModificationDateKey]).contentModificationDate) ?? .distantPast) >
                          ((try? $1.resourceValues(forKeys: [.contentModificationDateKey]).contentModificationDate) ?? .distantPast) }
            files += mine.prefix(5)
            let crashes = driverCrashes(sinceInstallOnly: false)
            if !crashes.isEmpty {
                let cr = dir.appendingPathComponent("crash-report.txt")
                if writeReport(Array(crashes.prefix(3)), to: cr) { files.append(cr) }
            }
            var ids: [String] = [], errs: [String] = []
            if err != nil { errs.append("Some system logs could not be collected. See collect.txt for details.") }
            // Directory enumeration is unordered: a stick with many boot logs could crowd out
            // the GPU state, kernel log, or crash report. Always send those first.
            let important = ["driver-state.txt", "driver-kernel-log.txt", "driver-plugin-log.txt", "crash-report.txt", "collect.txt", "driver-update-log.txt"]
            files.sort {
                let a = $0.lastPathComponent.hasPrefix("macos-WindowServer") ? 3 : (important.firstIndex(of: $0.lastPathComponent) ?? important.count)
                let b = $1.lastPathComponent.hasPrefix("macos-WindowServer") ? 3 : (important.firstIndex(of: $1.lastPathComponent) ?? important.count)
                return a == b ? $0.lastPathComponent < $1.lastPathComponent : a < b
            }
            for f in files.prefix(12) {
                guard var data = try? Data(contentsOf: f), !data.isEmpty else { continue }
                data.removeAll { $0 == 0 }   // the site refuses a text log with NUL bytes (OpenCore pads its log file)
                let text = redact(String(decoding: data, as: UTF8.self))
                let body = Data(text.utf8)
                let sha = SHA256.hash(data: body).map { String(format: "%02x", $0) }.joined()
                var req = URLRequest(url: URL(string: "https://nullmothsystems.com/api/upload")!, timeoutInterval: 60)
                req.httpMethod = "POST"; req.httpBody = body
                req.setValue("application/octet-stream", forHTTPHeaderField: "Content-Type")
                let name = f.lastPathComponent.hasSuffix(".txt") || f.lastPathComponent.hasSuffix(".log") ? f.lastPathComponent : f.lastPathComponent + ".txt"
                req.setValue(name.addingPercentEncoding(withAllowedCharacters: .alphanumerics) ?? "log.txt", forHTTPHeaderField: "X-File-Name")
                let meta = try! JSONSerialization.data(withJSONObject: ["consent": true, "notes": "1401 Mac \(Bundle.main.infoDictionary?["CFBundleShortVersionString"] as? String ?? "?") (driver \(Package.version)) logs (sent from the app)", "batch": "1401-mac"])
                req.setValue(meta.base64EncodedString().replacingOccurrences(of: "+", with: "-").replacingOccurrences(of: "/", with: "_").replacingOccurrences(of: "=", with: ""), forHTTPHeaderField: "X-Meta")
                req.setValue(sha, forHTTPHeaderField: "X-Content-SHA256")
                let done = DispatchSemaphore(value: 0)
                URLSession.shared.dataTask(with: req) { d, _, e in
                    if let d = d, let j = try? JSONSerialization.jsonObject(with: d) as? [String: Any] {
                        if let id = j["id"] as? String, (j["sha256"] as? String ?? sha) == sha { ids.append(id) }
                        else { errs.append("\(name): \(j["error"] as? String ?? "refused")") }
                    } else { errs.append("\(name): \(e?.localizedDescription ?? "no answer")") }
                    done.signal()
                }.resume()
                _ = done.wait(timeout: .now() + 90)
            }
            try? fm.removeItem(at: dir)
            DispatchQueue.main.async { self.send("logsDone", ["ok": !ids.isEmpty, "ids": ids, "errors": errs]) }
        }
    }

    func crashReportFromWindow() {
        let cr = driverCrashes(sinceInstallOnly: false)
        guard !cr.isEmpty else { send("crashDone", ["ok": false, "why": "No crash that names the driver was found."]); return }
        let f = DateFormatter(); f.dateFormat = "yyyy-MM-dd-HHmm"
        let dst = FileManager.default.urls(for: .desktopDirectory, in: .userDomainMask)[0].appendingPathComponent("1401-crash-\(f.string(from: Date())).txt")
        if writeReport(cr, to: dst) {
            NSWorkspace.shared.activateFileViewerSelecting([dst]); NSWorkspace.shared.open(uploadPage)
            send("crashDone", ["ok": true, "path": dst.path])
        } else { send("crashDone", ["ok": false, "why": "Could not write the report to the Desktop."]) }
    }

    func run(mode: String, pkg: String, efi: String, extra: [String], expectedSha: String? = nil) {
        guard !runningSetup, pendingDownload == nil else { return }
        runningSetup = true
        try? FileManager.default.createDirectory(at: logs, withIntermediateDirectories: true)
        let stamp = ISO8601DateFormatter().string(from: Date()).replacingOccurrences(of: ":", with: "")
        let log = logs.appendingPathComponent("setup-\(mode)-\(stamp).log")
        FileManager.default.createFile(atPath: log.path, contents: nil)
        let res = Bundle.main.resourceURL!
        var args: [String]
        switch mode {
        case "remove": args = ["--remove"]
        case "usbmap", "verbose", "update": args = extra
        default:
            args = ["--pkg", pkg, "--sha", expectedSha ?? Package.sha256, "--tool", res.appendingPathComponent("NullMothSafe.efi").path,
                    "--app", Bundle.main.executablePath ?? ""]
            if mode == "dry" { args.append("--dry") }
            // per-system rules: this machine's profile picks the rules (nullmoth-rules.json); their knobs go to the setup script
            let prof = App.machineProfile(pciDisplays())
            let sel = Profile.select(App.rules(), prof)
            for (k, v) in sel.conf.sorted(by: { $0.key < $1.key }) { args += ["--knob", "\(k)=\(v)"] }
            var record = prof; record["rules"] = sel.ids
            let pf = FileManager.default.temporaryDirectory.appendingPathComponent("nullmoth-profile-\(UUID().uuidString).json")
            if let d = try? JSONSerialization.data(withJSONObject: record, options: [.prettyPrinted, .sortedKeys]), (try? d.write(to: pf)) != nil {
                args += ["--profile", pf.path]
            }
            args += extra
        }
        if efi != "auto", !efi.isEmpty { args += ["--efi", efi] }
        let q = { (s: String) in "'" + s.replacingOccurrences(of: "'", with: "'\\''") + "'" }
        let cmd = "/bin/bash \(q(res.appendingPathComponent("nullmoth-setup.sh").path)) \(args.map(q).joined(separator: " ")) >> \(q(log.path)) 2>&1"
        let asrc = "do shell script \"\(cmd.replacingOccurrences(of: "\\", with: "\\\\").replacingOccurrences(of: "\"", with: "\\\""))\" with administrator privileges"
        send("run", ["state": "start", "mode": mode, "log": log.path])
        DispatchQueue.global().async {
            let done = DispatchSemaphore(value: 0)
            var sent = 0
            DispatchQueue.global().async {
                while done.wait(timeout: .now() + 0.3) == .timedOut { sent = self.flush(log, from: sent) }
            }
            var err: NSDictionary?
            NSAppleScript(source: asrc)?.executeAndReturnError(&err)
            done.signal()
            Thread.sleep(forTimeInterval: 0.4)
            sent = self.flush(log, from: sent)
            let cancelled = (err?[NSAppleScript.errorNumber] as? Int) == -128
            let text = (try? String(contentsOf: log, encoding: .utf8)) ?? ""
            let result = text.split(separator: "\n").last(where: { $0.hasPrefix("RESULT ") })
            let succeeded = err == nil && result == "RESULT ok"
            DispatchQueue.main.async {
                self.runningSetup = false
                if cancelled { self.send("run", ["state": "cancelled", "mode": mode]) }
                else { self.send("run", ["state": "end", "mode": mode, "ok": succeeded, "log": log.path]) }
            }
        }
    }
    func flush(_ log: URL, from: Int) -> Int {
        guard let t = try? String(contentsOf: log, encoding: .utf8) else { return from }
        let lines = t.split(separator: "\n", omittingEmptySubsequences: false).dropLast()
        if lines.count > from { send("lines", Array(lines[from...]).map(String.init)) }
        return max(from, lines.count)
    }
}

let argv = CommandLine.arguments
func table() -> [String: Any] {
    guard let d = try? Data(contentsOf: Bundle.main.resourceURL!.appendingPathComponent("nvidia_gsp_ids.json")) else { return [:] }
    return (try? JSONSerialization.jsonObject(with: d) as? [String: Any]) ?? [:]
}
if argv.contains("--scan") { print(json(App.scanMac(table()))); exit(0) }
if argv.contains("--usb") { print(json(usbControllers())); exit(0) }
if let i = argv.firstIndex(of: "--crash-report"), i + 1 < argv.count {
    let cr = driverCrashes(sinceInstallOnly: false)
    let ok = !cr.isEmpty && writeReport(cr, to: URL(fileURLWithPath: argv[i + 1]))
    print(json(["crashes": cr.map(\.lastPathComponent), "written": ok])); exit(ok ? 0 : 1)
}
if argv.contains("--crash-check") { exit(crashCheck(ui: !argv.contains("--no-ui"))) }
let app = NSApplication.shared
let delegate = App()
app.delegate = delegate
app.setActivationPolicy(.regular)
app.run()
