// 1401
// Copyright (c) 2026 NullMoth Systems. SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0

import Cocoa
import CryptoKit
import IOKit
import Metal
import WebKit
import UniformTypeIdentifiers

struct Package {
    static let version = "1.11.0"
    static let name = "nullmoth-nvidia-1.11.0.tar.gz"
    static let url = URL(string: "https://github.com/nullmoth/nvidia-macos-driver/releases/download/v1.11.0/nullmoth-nvidia-1.11.0.tar.gz")!
    static let sha256 = "6e568e28ee34c3b41f4dfda980fd8feb50fdd0a69cf6d0f6e54338873b8ec2e7"
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
    HardwareMap.integer(prop(e, k), maximum: UInt64(UInt32.max)).map { UInt32($0) }
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
        var path = [CChar](repeating: 0, count: 512)
        let located = IORegistryEntryGetPath(s, kIOServicePlane, &path) == KERN_SUCCESS
        var identity = Profile.displayIdentity(vendor: v, device: d,
                                               subsystemVendor: u32(s, "subsystem-vendor-id"),
                                               subsystemDevice: u32(s, "subsystem-id"),
                                               path: located ? String(cString: path) : "")
        identity["model"] = str(s, "model") ?? ""
        return identity
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
                              "connector": Int(u32(p, "UsbConnector") ?? 255), "connectorKnown": u32(p, "UsbConnector") != nil, "devices": names,
                              "comment": str(p, "#comment") ?? ""])
                devs.forEach { IOObjectRelease($0) }
            }
            IOObjectRelease(p)
        }
        out.append(["controller": regName(parent), "key": String(cString: path), "vendor": String(format: "%04X", ven), "device": String(format: "%04X", dev),
                    "path": String(cString: path), "ports": ports.sorted { ($0["port"] as! Int) < ($1["port"] as! Int) }])
        IOObjectRelease(parent); IOObjectRelease(x)
    }
    return out
}

func usbControllerKey(_ c: [String: Any]) -> String {
    let key = c["key"] as? String ?? ""
    return key.isEmpty ? (c["controller"] as? String ?? "") : key
}

func utbMap(_ ctrls: [[String: Any]], _ sel: [String: [String: Int]]) -> ([String: Any]?, String?) {
    var pers: [String: Any] = [:]
    let keys = Set(ctrls.map(usbControllerKey))
    if sel.contains(where: { !keys.contains($0.key) && !$0.value.isEmpty }) { return (nil, "A selected USB controller is no longer present. Scan again.") }
    var matches = Set<String>()
    for c in ctrls {
        let name = c["controller"] as! String
        let key = usbControllerKey(c)
        guard let chosen = sel[key], !chosen.isEmpty else { continue }
        if chosen.count > 15 { return (nil, "\(name) has \(chosen.count) ports picked; macOS allows 15 per controller. Untick some.") }
        let id = "0x\(c["device"]!)\(c["vendor"]!)"
        let path = c["path"] as? String ?? ""
        let sameId = ctrls.filter { "0x\($0["device"]!)\($0["vendor"]!)" == id }.count
        if sameId > 1 && path.isEmpty { return (nil, "USB controllers share the PCI id \(id), but their registry paths are unavailable. Scan again.") }
        let match = path.isEmpty ? id : path
        if !matches.insert(match).inserted { return (nil, "Two selected USB controllers have the same identity. Scan again.") }
        var ports: [String: Any] = [:]
        var highest: UInt32 = 0
        var numbers = Set<UInt32>()
        for p in c["ports"] as! [[String: Any]] {
            let pn = p["name"] as! String
            guard let conn = chosen[pn] else { continue }
            guard let number = p["port"] as? Int, number > 0, let value = UInt32(exactly: number), numbers.insert(value).inserted,
                  [0, 3, 9, 10, 255].contains(conn) else { return (nil, "\(name) has an invalid port or connector selection. Scan again.") }
            highest = max(highest, value)
            var n = value.littleEndian
            ports[pn] = ["port": Data(bytes: &n, count: 4), "UsbConnector": conn,
                         "#comment": ((p["devices"] as? [String]) ?? []).joined(separator: ", ")]
        }
        if ports.count != chosen.count { return (nil, "Some selected ports on \(name) are no longer present. Scan again.") }
        var top = highest.littleEndian
        var personality: [String: Any] = ["CFBundleIdentifier": "com.dhinakg.USBToolBox.kext", "IOClass": "USBToolBox", "IOMatchCategory": "USBToolBox",
                      "IOPCIPrimaryMatch": id, "IOProviderClass": "IOPCIDevice",
                      "IOProviderMergeProperties": ["ports": ports, "port-count": Data(bytes: &top, count: 4)]]
        if !path.isEmpty { personality["IOPathMatch"] = path }
        pers["Controller-\(pers.count)"] = personality
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
let supportCrashProcesses = ["firefox", "plugin-container", "Blender"]
func supportCrashRank(_ name: String) -> Int? {
    if name.hasPrefix("macos-WindowServer") { return 3 }
    if supportCrashProcesses.contains(where: { name.lowercased().hasPrefix("macos-" + $0.lowercased() + "-") }) { return 3 }
    return nil
}

func userApplicationCrashes() -> [URL] {
    let fm = FileManager.default
    let dir = URL(fileURLWithPath: crashDirs[1])
    let files = ((try? fm.contentsOfDirectory(at: dir, includingPropertiesForKeys: [.contentModificationDateKey])) ?? [])
        .filter { $0.pathExtension == "ips" }
        .sorted { ((try? $0.resourceValues(forKeys: [.contentModificationDateKey]).contentModificationDate) ?? .distantPast) >
                  ((try? $1.resourceValues(forKeys: [.contentModificationDateKey]).contentModificationDate) ?? .distantPast) }
    return supportCrashProcesses.compactMap { process in
        files.first { $0.lastPathComponent.lowercased().hasPrefix(process.lowercased() + "-") }
    }
}
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
    let user = NSUserName(), full = NSFullUserName()
    let homes = [NSHomeDirectory(), URL(fileURLWithPath: "/Users").appendingPathComponent(user).path]
    var exact: [String: String] = [:]
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
    return redactSupportText(s, homes: homes, identities: exact)
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
    var usbGeneration = 0

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
        var payload = data
        let state = (data as? [String: Any])?["state"] as? String
        if (["dl", "upd"].contains(event) && ["start", "error"].contains(state ?? "")) || event == "usbErr" {
            do {
                let directory = try SavedReports.session(in: logs.appendingPathComponent("Reports"))
                let record: [String: Any] = ["event": event, "data": data,
                    "app_version": Bundle.main.infoDictionary?["CFBundleShortVersionString"] as? String ?? "unavailable",
                    "helper_version": Package.version, "helper_sha256": Package.sha256,
                    "os_version": ProcessInfo.processInfo.operatingSystemVersionString,
                    "time": ISO8601DateFormatter().string(from: Date())]
                _ = try SavedReports.create(Data(redact(json(record)).utf8), in: directory, name: "setup-" + event + ".txt")
            } catch {
                let warning = "The local operation report could not be saved: \(error.localizedDescription)"
                if var fields = data as? [String: Any] {
                    fields["why"] = ((fields["why"] as? String).map { $0 + " " } ?? "") + warning
                    payload = fields
                } else { payload = "\(data) \(warning)" }
            }
        }
        guard let j = try? JSONSerialization.data(withJSONObject: ["event": event, "data": payload]),
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
        case "run":
            guard let mode = AppActions.directRunMode(b["mode"]) else { return }
            run(mode: mode, pkg: b["pkg"] as? String ?? "", efi: b["efi"] as? String ?? "auto", extra: [])
        case "restart": NSAppleScript(source: "tell application \"System Events\" to restart")?.executeAndReturnError(nil)
        case "privacy": NSWorkspace.shared.open(URL(string: "x-apple.systempreferences:com.apple.preference.security?Security")!)
        case "logs": try? FileManager.default.createDirectory(at: logs, withIntermediateDirectories: true); NSWorkspace.shared.open(logs)
        case "open": if let u = b["url"] as? String, let url = URL(string: u), url.scheme == "https" { NSWorkspace.shared.open(url) }
        case "usbStart": usbWatch(true)
        case "usbStop": usbWatch(false)
        case "usbWrite": usbWrite(b["sel"] as? [String: [String: Int]] ?? [:], efi: b["efi"] as? String ?? "auto")
        case "crashReport": crashReportFromWindow()
        case "sendLogs": sendLogs(sample: b["sample"] as? String ?? "")
        case "optionalDiagnostics": optionalDiagnostics()
        case "importDiagnosticReceipt": importDiagnosticReceipt()
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
    /// Live properties select per-system rules. Attached Windows profiles are
    /// diagnostic evidence until their identity is bound to this boot configuration.
    static func machineProfile(_ gpus: [[String: Any]]) -> [String: Any] {
        var p: [String: Any] = [:]
        if let nv = gpus.first(where: { $0["vendor"] as? String == "10DE" }), let dev = nv["device"] as? String {
            p["gpu_id"] = dev.uppercased(); p["arch"] = Profile.arch(dev)
        }
        let vendor = sysctl("machdep.cpu.vendor")
        p["cpu_vendor"] = vendor.contains("AMD") ? "amd" : vendor.contains("Intel") ? "intel" : vendor
        p["cpu"] = sysctl("machdep.cpu.brand_string")
        let bat = IOServiceGetMatchingService(kIOMainPortDefault, IOServiceMatching("AppleSmartBattery"))
        var installed: Bool? = nil
        if bat != 0 {
            if let value = prop(bat, "BatteryInstalled") as? NSNumber, CFGetTypeID(value) == CFBooleanGetTypeID() {
                installed = value.boolValue
            }
            IOObjectRelease(bat)
        }
        Profile.attachChassisEvidence(servicePresent: bat != 0, batteryInstalled: installed, to: &p)
        p["egpu"] = Profile.nvidiaBehindThunderbolt()
        p["macos_major"] = ProcessInfo.processInfo.operatingSystemVersion.majorVersion
        p["gpu_candidates"] = gpus
        p["gpu_rule_selection"] = gpus.filter { $0["vendor"] as? String == "10DE" }.count > 1 ? "first_registry_device_multiple_present" : "single_registry_device"
        var windowsProfiles: [[String: Any]] = []
        for v in ((try? FileManager.default.contentsOfDirectory(atPath: "/Volumes")) ?? []).sorted() {
            if let d = try? Data(contentsOf: URL(fileURLWithPath: "/Volumes/\(v)/NullMoth/system-profile.json")),
               let j = try? JSONSerialization.jsonObject(with: d) as? [String: Any] {
                windowsProfiles.append(j)
            }
        }
        Profile.attachWindowsDiagnostics(windowsProfiles, to: &p)
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
    static func releaseDriverVersion(_ name: String) -> String? {
        guard let match = name.range(of: #"^nullmoth-nvidia-([0-9]+\.[0-9]+(?:\.[0-9]+)?)\.tar\.gz$"#, options: .regularExpression) else { return nil }
        return String(name[match].dropFirst("nullmoth-nvidia-".count).dropLast(".tar.gz".count))
    }
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
                if let version = App.releaseDriverVersion(n), pkg == nil || App.newer(version, than: pkg!.0) {
                    pkg = (version, n, u)
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
        usbGeneration += 1
        usbTimer?.invalidate(); usbTimer = nil
        if on {
            seenUsb = [:]
            usbTimer = Timer.scheduledTimer(withTimeInterval: 1.0, repeats: true) { _ in self.usbTick() }
            usbTick()
        }
    }
    func usbTick() {
        let generation = usbGeneration
        DispatchQueue.global().async {
            let c = usbControllers()
            DispatchQueue.main.async {
                guard self.usbGeneration == generation else { return }
                for ctl in c {
                    let n = usbControllerKey(ctl)
                    for p in ctl["ports"] as! [[String: Any]] where !((p["devices"] as? [String]) ?? []).isEmpty {
                        self.seenUsb[n, default: []].insert(p["name"] as! String)
                    }
                }
                self.send("usb", ["controllers": c, "seen": self.seenUsb.mapValues { Array($0) }])
            }
        }
    }
    func usbWrite(_ sel: [String: [String: Int]], efi: String) {
        let (plist, err) = utbMap(usbControllers(), sel)
        guard let plist else { send("usbErr", err ?? "could not build the map"); return }
        let dir = FileManager.default.temporaryDirectory.appendingPathComponent("nullmoth-utb-\(UUID().uuidString)/UTBMap.kext/Contents")
        try? FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        guard let d = try? PropertyListSerialization.data(fromPropertyList: plist, format: .xml, options: 0),
              (try? d.write(to: dir.appendingPathComponent("Info.plist"))) != nil else { send("usbErr", "could not write the map"); return }
        // the script wants the folder that HOLDS UTBMap.kext (10-07: passing the .kext itself failed every map)
        run(mode: "usbmap", pkg: "", efi: efi, extra: ["--usbmap", dir.deletingLastPathComponent().deletingLastPathComponent().path])
    }

    func optionalDiagnostics() {
        guard let resources = Bundle.main.resourceURL else { return }
        send("diagnosticsStatus", ["text": "Checking availability. No trace starts."])
        DispatchQueue.global().async {
            let receipt = OptionalDiagnostics.prerequisites(executable: resources.appendingPathComponent("nullmoth-diagnostics-preflight"), targetPID: getpid())
            self.send("diagnosticsStatus", ["text": OptionalDiagnostics.guidance(receipt)])
        }
    }

    func importDiagnosticReceipt() {
        let panel = NSOpenPanel(); panel.title = "Choose a saved diagnostics receipt"
        panel.allowsMultipleSelection = false; panel.canChooseDirectories = false
        panel.allowedContentTypes = [.json]
        guard panel.runModal() == .OK, let url = panel.url else { return }
        do {
            let data = try DiagnosticReceipts.privateFile(url)
            _ = try DiagnosticReceipts.validatedSummary(data)
            try FileManager.default.createDirectory(at: support, withIntermediateDirectories: true)
            try DiagnosticReceipts.stage(data, directory: support.appendingPathComponent("Diagnostics", isDirectory: true))
            send("diagnosticReceipt", ["text": "Saved receipt accepted. Only its validated count/configuration summary can be sent, after the separate Send logs confirmation. Nothing has been uploaded."])
        } catch {
            send("diagnosticReceipt", ["text": "Receipt refused. Choose an unchanged supported JSON receipt saved with this session's upload permission, owned by this account, with private file permissions (0600). Other logs remain available."])
        }
    }

    // "Send logs" (two clicks: the button, then Send). Collects 1401's own logs, the driver's state, a redacted crash
    // report when the driver crashed, and - after macOS asks for the password - OpenCore's boot logs and saved panics
    // from every OpenCore partition (sticks included). Every file is uploaded with its SHA-256, and the site refuses
    // any upload whose bytes differ from it.
    // sample: "" = the ordinary logs; "flicker" = first 20 s of display timing taken while the screen flickers.
    // It rides the same collector, consent and upload as Send logs.
    func sendLogs(sample: String = "") {
        guard ["", "flicker"].contains(sample) else { return }
        let a = NSAlert()
        a.messageText = sample == "flicker" ? "Record the flicker and send it to NullMoth" : "Send logs to NullMoth"
        a.informativeText = "1401 is sending this Mac's NullMoth logs to nullmothsystems.com so the problem can be found and fixed: what 1401 did, the driver's state, driver crash reports, recent WindowServer, Firefox and Blender crash reports, update diagnostics, OpenCore's startup logs, and a numeric device/CPU relationship map. A separately imported diagnostics receipt is included only if that session allowed upload, and only as a validated count/configuration summary. The map records kernel-visible hardware, not verified driver support. Your name, your Mac's name, serial numbers and addresses are removed first. macOS asks for your password so 1401 can read the startup logs."
        if sample == "flicker" {
            a.informativeText = "Start this while the screen is flickering. For 20 seconds 1401 records how the driver puts frames on the display (flip counts and timing, refused updates, the refresh rate and WindowServer's load), then sends it with the ordinary logs. Keep using the Mac normally while it records.\n\n" + a.informativeText
        }
        a.addButton(withTitle: "Send"); a.addButton(withTitle: "Cancel")
        guard a.runModal() == .alertFirstButtonReturn else { send("logsDone", ["ok": false, "why": "Not sent."]); return }
        DispatchQueue.global().async {
            let fm = FileManager.default
            let dir: URL
            do {
                dir = try SavedReports.session(in: logs.appendingPathComponent("Reports"))
                _ = try SavedReports.create(Data("Log collection started. Files are retained locally after upload.\n".utf8), in: dir, name: "session.txt")
            } catch {
                DispatchQueue.main.async { self.send("logsDone", ["ok": false, "why": "Could not save a local report: \(error.localizedDescription)"]) }
                return
            }
            let script = Bundle.main.resourceURL!.appendingPathComponent("nullmoth-log-capture.sh").path
            let q = { (x: String) in "'" + x.replacingOccurrences(of: "'", with: "'\\''") + "'" }
            let cmd = (sample.isEmpty ? "" : "NULLMOTH_SAMPLE=\(sample) ") + "/bin/bash \(q(script))"
            var collectionErrors: [String] = []
            DispatchQueue.main.async {
                self.send("logsStatus", ["text": "Waiting for your Mac password (a macOS window asks for it). Collecting takes a few minutes; 1401 keeps working."])
                NSApp.activate(ignoringOtherApps: true)
            }
            // off the main thread: the app stays responsive while the logs are collected (Privileged.swift)
            let run = Privileged.run(cmd, timeout: 15 * 60)
            if run.cancelled {
                DispatchQueue.main.async { self.send("logsDone", ["ok": false, "why": "Not sent: the password prompt was cancelled."]) }
                return
            }
            DispatchQueue.main.async { self.send("logsStatus", ["text": "Sending..."]) }
            do { collectionErrors += try SavedReports.importCollection(run.output, to: dir) }
            catch { collectionErrors.append("System log collection was incomplete: \(error.localizedDescription)") }
            let err = run.error
            if let error = err { collectionErrors.append(redact(error)) }
            if !collectionErrors.isEmpty {
                do { _ = try SavedReports.create(Data(collectionErrors.joined(separator: "\n").utf8), in: dir, name: "collection-errors.txt") }
                catch { collectionErrors.append("The collector error summary could not be saved.") }
            }
            if let executable = Bundle.main.executableURL {
                let map = HardwareMapWorker.run(executable: executable)
                let target = dir.appendingPathComponent("hardware-map.json")
                if let data = try? JSONSerialization.data(withJSONObject: map, options: [.prettyPrinted, .sortedKeys]) {
                    try? data.write(to: target, options: .atomic)
                    try? fm.setAttributes([.posixPermissions: 0o600], ofItemAtPath: target.path)
                }
            }
            var files = ((try? fm.contentsOfDirectory(at: dir, includingPropertiesForKeys: nil)) ?? [])
            let sessions = (try? fm.contentsOfDirectory(at: logs.appendingPathComponent("Reports"), includingPropertiesForKeys: [.contentModificationDateKey])) ?? []
            let retained = sessions.filter { $0 != dir }.flatMap { session in
                ((try? fm.contentsOfDirectory(at: session, includingPropertiesForKeys: [.contentModificationDateKey])) ?? [])
                    .filter { $0.lastPathComponent.hasPrefix("setup-") || $0.lastPathComponent.hasPrefix("pending-") }
            }
            let mine = ((try? fm.contentsOfDirectory(at: logs, includingPropertiesForKeys: [.contentModificationDateKey])) ?? [])
                .sorted { ((try? $0.resourceValues(forKeys: [.contentModificationDateKey]).contentModificationDate) ?? .distantPast) >
                          ((try? $1.resourceValues(forKeys: [.contentModificationDateKey]).contentModificationDate) ?? .distantPast) }
            let pendingFiles = retained.filter { $0.lastPathComponent.hasPrefix("pending-") }
            files += (mine + retained.filter { !$0.lastPathComponent.hasPrefix("pending-") })
                .filter { (try? $0.resourceValues(forKeys: [.isRegularFileKey]).isRegularFile) == true }
                .sorted {
                    let a = $0.lastPathComponent.hasPrefix("pending-")
                    let b = $1.lastPathComponent.hasPrefix("pending-")
                    if a != b { return a }
                    return ((try? $0.resourceValues(forKeys: [.contentModificationDateKey]).contentModificationDate) ?? .distantPast) <
                           ((try? $1.resourceValues(forKeys: [.contentModificationDateKey]).contentModificationDate) ?? .distantPast)
                }
                .prefix(24)
            for crash in userApplicationCrashes() {
                let target = dir.appendingPathComponent("macos-" + crash.lastPathComponent)
                if (try? fm.copyItem(at: crash, to: target)) != nil { files.append(target) }
            }
            let crashes = driverCrashes(sinceInstallOnly: false)
            if !crashes.isEmpty {
                let cr = dir.appendingPathComponent("crash-report.txt")
                if writeReport(Array(crashes.prefix(3)), to: cr) { files.append(cr) }
            }
            var ids: [String] = [], errs: [String] = collectionErrors, acknowledgements: [[String: String]] = []
            let savedReceipt = support.appendingPathComponent("Diagnostics/diagnostic-receipt.json")
            if fm.fileExists(atPath: savedReceipt.path) {
                do {
                    let data = try DiagnosticReceipts.privateFile(savedReceipt)
                    let summary = try DiagnosticReceipts.sanitized(data, sendLogsConsent: true)
                    let target = dir.appendingPathComponent("diagnostic-session.json")
                    try summary.write(to: target, options: .atomic)
                    try fm.setAttributes([.posixPermissions: 0o600], ofItemAtPath: target.path)
                    files.append(target)
                } catch { errs.append("The optional diagnostics receipt was refused; ordinary logs are retained.") }
            }
            if err != nil { errs.append("Some system logs could not be collected. See collect.txt for details.") }
            // Directory enumeration is unordered: a stick with many boot logs could crowd out
            // the GPU state, kernel log, or crash report. Always send those first.
            let important = ["hardware-map.json", "driver-state.txt", "driver-kernel-log.txt", "driver-display.txt", "previous-boot-kernel-log.txt", "driver-plugin-log.txt", "crash-report.txt", "driver-wsreset.log.txt", "diagnostic-session.json", "collect.txt", "driver-update-log.txt"]
            files.sort {
                let a = supportCrashRank($0.lastPathComponent) ?? (important.firstIndex(of: $0.lastPathComponent) ?? important.count)
                let b = supportCrashRank($1.lastPathComponent) ?? (important.firstIndex(of: $1.lastPathComponent) ?? important.count)
                let pendingA = $0.lastPathComponent.hasPrefix("pending-")
                let pendingB = $1.lastPathComponent.hasPrefix("pending-")
                if pendingA != pendingB { return pendingA }
                return a == b ? $0.lastPathComponent < $1.lastPathComponent : a < b
            }
            // One random value per send, shared by every file of it, so the site can group one run's logs. It names the
            // run, not the Mac: a new one each time.
            let batch = newUploadBatch()
            let prepared: ReportQueue.Prepared
            do {
                prepared = try ReportQueue.prepare(files: files, pendingFiles: pendingFiles, in: dir) { data in
                    var bytes = data; bytes.removeAll { $0 == 0 }
                    return Data(redact(String(decoding: bytes, as: UTF8.self)).utf8)
                }
            } catch {
                DispatchQueue.main.async { self.send("logsDone", ["ok": false, "why": "Could not save the report queue: \(error.localizedDescription)", "localPath": dir.path]) }
                return
            }
            errs += prepared.warnings
            let selected = ReportQueue.select(prepared.entries)
            if prepared.entries.count > selected.count { errs.append("More reports are saved for the next Send logs attempt.") }
            for entry in selected {
                let body = entry.bytes, sha = entry.sha, name = entry.name
                var req = URLRequest(url: URL(string: "https://nullmothsystems.com/api/upload")!, timeoutInterval: 60)
                req.httpMethod = "POST"; req.httpBody = body
                req.setValue("application/octet-stream", forHTTPHeaderField: "Content-Type")
                req.setValue(name.addingPercentEncoding(withAllowedCharacters: .alphanumerics) ?? "log.txt", forHTTPHeaderField: "X-File-Name")
                let meta = try! JSONSerialization.data(withJSONObject: ["consent": true, "notes": "1401 Mac \(Bundle.main.infoDictionary?["CFBundleShortVersionString"] as? String ?? "?") (driver \(Package.version)) logs (sent from the app)", "batch": batch])
                req.setValue(meta.base64EncodedString().replacingOccurrences(of: "+", with: "-").replacingOccurrences(of: "/", with: "_").replacingOccurrences(of: "=", with: ""), forHTTPHeaderField: "X-Meta")
                req.setValue(sha, forHTTPHeaderField: "X-Content-SHA256")
                let done = DispatchSemaphore(value: 0)
                let result = ReportUploadResult()
                let task = URLSession.shared.dataTask(with: req) { data, response, error in
                    result.finish(data: data, response: response, error: error, sha256: sha)
                    done.signal()
                }
                task.resume()
                let timedOut = done.wait(timeout: .now() + 90) == .timedOut
                if timedOut { task.cancel() }
                let outcome = result.take(timedOut: timedOut)
                if let id = outcome.0 {
                    ids.append(id); acknowledgements.append(["name": name, "sha256": sha, "id": id])
                    do { try ReportQueue.confirm(entry, id: id, in: dir) }
                    catch { errs.append("\(name): confirmed, but its local retry marker could not be updated.") }
                }
                if let error = outcome.1 { errs.append("\(name): \(error)") }
            }
            do {
                let receipt = try JSONSerialization.data(withJSONObject: ["batch": batch, "confirmed": acknowledgements, "errors": errs], options: [.prettyPrinted, .sortedKeys])
                _ = try SavedReports.create(receipt, in: dir, name: "upload-receipt.json")
            } catch { errs.append("The upload receipt could not be saved; collected files remain locally.") }
            DispatchQueue.main.async { self.send("logsDone", ["ok": !ids.isEmpty, "ids": ids, "errors": errs, "localPath": dir.path]) }
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
        guard ["dry", "install", "remove", "usbmap", "verbose"].contains(mode),
              !runningSetup, pendingDownload == nil else { return }
        runningSetup = true
        let log: URL
        do {
            let directory = try SavedReports.session(in: logs.appendingPathComponent("Reports"))
            log = try SavedReports.create(Data("Setup \(mode) started.\n".utf8), in: directory, name: "setup-\(mode).log")
        } catch {
            runningSetup = false
            send("run", ["state": "end", "mode": mode, "ok": false, "why": "Could not save a local report: \(error.localizedDescription)"])
            return
        }
        let res = Bundle.main.resourceURL!
        var args: [String]
        switch mode {
        case "remove": args = ["--remove"]
        case "usbmap", "verbose": args = extra
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
        let cmd = "/bin/bash \(q(res.appendingPathComponent("nullmoth-operation-capture.sh").path)) \(args.map(q).joined(separator: " "))"
        send("run", ["state": "start", "mode": mode, "log": log.path])
        NSApp.activate(ignoringOtherApps: true)
        DispatchQueue.global().async {
            var captureFailed = false
            // osascript in its own process (Privileged.swift): WAS NSAppleScript on this background queue, where its
            // password panel could hang unseen and the window sat on "Finding the OpenCore partition"
            let run = Privileged.run(cmd, timeout: 30 * 60)
            let err = run.error
            if !run.cancelled {
                do {
                    let directory = log.deletingLastPathComponent()
                    let warnings = try SavedReports.importCollection(run.output, to: directory)
                    for name in ["setup-output.txt", "setup-output-head.txt", "setup-output-tail.txt"] {
                        let file = directory.appendingPathComponent(name)
                        if FileManager.default.fileExists(atPath: file.path) {
                            try SavedReports.append(Data(contentsOf: file), to: log)
                        }
                    }
                    for warning in warnings { try SavedReports.append(Data(("\n" + warning + "\n").utf8), to: log) }
                    captureFailed = !warnings.isEmpty
                } catch {
                    captureFailed = true
                    let message = "Could not retain setup output: \(error.localizedDescription)"
                    do { _ = try SavedReports.create(Data(message.utf8), in: log.deletingLastPathComponent(), name: "setup-capture-error.txt") }
                    catch { self.send("lines", ["The capture error could not be saved: \(error.localizedDescription)"]) }
                    self.send("lines", [message])
                }
            } else {
                captureFailed = true
            }
            _ = self.flush(log, from: 0)
            let cancelled = run.cancelled
            if let error = err {
                let message = redact(error)
                do { _ = try SavedReports.create(Data(message.utf8), in: log.deletingLastPathComponent(), name: "setup-launch-error.txt") }
                catch { DispatchQueue.main.async { self.send("lines", ["Could not save the setup error: \(error.localizedDescription)"]) } }
            }
            let text = (try? String(contentsOf: log, encoding: .utf8)) ?? ""
            let result = text.split(separator: "\n").last(where: { $0.hasPrefix("RESULT ") })
            let succeeded = err == nil && !captureFailed && result == "RESULT ok"
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
if argv == [argv[0], "--hardware-map"] {
    var map = HardwareMap.collect()
    var bytesWritten = 0
    guard HardwareMapWorker.emitCheckpoint(map, phase: "baseline", bytesWritten: &bytesWritten) else { exit(75) }
    map["peripheral_facts"] = NativePeripheralFacts.collect()
    guard HardwareMapWorker.emitCheckpoint(map, phase: "final", bytesWritten: &bytesWritten) else { exit(75) }
    exit(0)
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
