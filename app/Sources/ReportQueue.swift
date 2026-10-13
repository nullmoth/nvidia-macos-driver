import Foundation
import CryptoKit
import Darwin

enum ReportQueue {
    struct Entry {
        let pending: URL, name: String, bytes: Data, sha: String, source: URL?, sourceSHA: String?
        let old: Bool, critical: Bool, date: Date
    }
    struct Prepared { var entries: [Entry] = []; var warnings: [String] = [] }
    // "Already sent" markers live in a private folder of their own. WAS: beside the sent file, so for the setup logs in
    // ~/Library/Logs/NullMoth (a 0755 folder) SavedReports.create refused every marker ("confirmed, but its local retry
    // marker could not be updated", 1401 #16) and the same files went up again in every batch from 1.6 to 1.13.
    static func sentMarkers() throws -> URL {
        let dir = FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask)[0]
            .appendingPathComponent("NullMoth/SentLogs", isDirectory: true)
        if !FileManager.default.fileExists(atPath: dir.path) {
            try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true, attributes: [.posixPermissions: 0o700])
        }
        try SavedReports.directory(dir)
        return dir
    }
    static func alreadySent(_ sourceSHA: String, besides file: URL) -> Bool {
        let name = "confirmed-source-" + sourceSHA + ".json"
        for dir in [try? sentMarkers(), file.deletingLastPathComponent()].compactMap({ $0 }) {
            if let saved = try? read(dir.appendingPathComponent(name), privateFile: true),
               let receipt = try? JSONSerialization.jsonObject(with: saved) as? [String: String],
               receipt["source_sha256"] == sourceSHA, !(receipt["id"] ?? "").isEmpty { return true }
        }
        return false
    }
    static func hash(_ data: Data) -> String { SHA256.hash(data: data).map { String(format: "%02x", $0) }.joined() }
    static func read(_ url: URL, privateFile: Bool) throws -> Data {
        let fd = open(url.path, O_RDONLY | O_NOFOLLOW | O_NONBLOCK | O_CLOEXEC)
        guard fd >= 0 else { throw SavedReports.Failure("A report file could not be opened.") }
        defer { close(fd) }
        var info = stat()
        guard fstat(fd, &info) == 0, info.st_mode & S_IFMT == S_IFREG,
              info.st_size >= 0, info.st_size <= 2 * 1024 * 1024,
              !privateFile || (info.st_uid == getuid() && info.st_mode & 0o077 == 0 && info.st_nlink == 1) else {
            throw SavedReports.Failure("A report is oversized or is not an allowed regular file.")
        }
        var data = Data(), buffer = [UInt8](repeating: 0, count: 65536)
        while true {
            let count = Darwin.read(fd, &buffer, buffer.count)
            if count < 0 && errno == EINTR { continue }
            guard count >= 0 else { throw SavedReports.Failure("A report could not be read.") }
            if count == 0 { break }
            guard data.count + count <= 2 * 1024 * 1024 else { throw SavedReports.Failure("A report exceeded its read limit.") }
            data.append(contentsOf: buffer.prefix(count))
        }
        var after = stat()
        guard fstat(fd, &after) == 0, after.st_size == info.st_size,
              after.st_mtimespec.tv_sec == info.st_mtimespec.tv_sec,
              after.st_mtimespec.tv_nsec == info.st_mtimespec.tv_nsec, data.count == Int(info.st_size) else {
            throw SavedReports.Failure("A report changed while being read.")
        }
        return data
    }
    static func prepare(files: [URL], pendingFiles: [URL], in session: URL,
                        redact: (Data) -> Data) throws -> Prepared {
        try SavedReports.directory(session)
        var result = Prepared(), seen = Set<URL>(), total = 0, index: [[String: String]] = []
        let all = files + pendingFiles
        if all.count > 256 { result.warnings.append("The queue reached its file limit; remaining originals are retained locally.") }
        for file in all.prefix(256) where seen.insert(file).inserted {
            do {
                let old = pendingFiles.contains(file)
                if old { try SavedReports.directory(file.deletingLastPathComponent()) }
                let raw = try read(file, privateFile: old)
                let sourceSHA = hash(raw)
                if alreadySent(sourceSHA, besides: file) {
                    // an old pending copy of a file whose original is already confirmed is retired, not sent again
                    if old { try? FileManager.default.moveItem(at: file, to: file.deletingLastPathComponent().appendingPathComponent("confirmed-" + file.lastPathComponent)) }
                    continue
                }
                let bytes = old ? raw : redact(raw)
                guard !bytes.isEmpty, bytes.count <= 2 * 1024 * 1024, total + bytes.count <= 32 * 1024 * 1024 else {
                    result.warnings.append("A report is empty or exceeds the queue limit; its original remains local."); continue
                }
                let privateName = String(decoding: redact(Data(file.lastPathComponent.utf8)), as: UTF8.self)
                let safeName = String(privateName.unicodeScalars.map { CharacterSet.alphanumerics.contains($0) || "._-".unicodeScalars.contains($0) ? Character($0) : "_" }.prefix(100))
                let name = safeName.hasSuffix(".txt") || safeName.hasSuffix(".log") ? safeName : safeName + ".txt"
                let pending = old ? file : try SavedReports.create(bytes, in: session, name: "pending-" + UUID().uuidString + "-" + name)
                // driver-wsreset = the flicker capture (1.4), -efi- = the user's own OpenCore config (1.6): neither reached
                // the server before 1.7 because 12 slots filled with other files first (10-10: 0 of them in any upload)
                let critical = ["hardware-map", "driver-state", "driver-kernel", "driver-plugin", "crash", "WindowServer", "Firefox", "Blender", "setup-", "collection-errors", "driver-wsreset", "-efi-"].contains { name.contains($0) }
                let date = (try? pending.resourceValues(forKeys: [.contentModificationDateKey]).contentModificationDate) ?? .distantPast
                var original: URL? = old ? nil : file, originalSHA: String? = old ? nil : sourceSHA
                if old, let metadata = try? read(file.deletingLastPathComponent().appendingPathComponent("queue-index.json"), privateFile: true),
                   let rows = try? JSONSerialization.jsonObject(with: metadata) as? [[String: String]], rows.count <= 256,
                   let row = rows.first(where: { $0["pending"] == file.lastPathComponent && $0["sha256"] == sourceSHA }),
                   let path = row["source"], let value = row["source_sha256"], value.range(of: #"^[0-9a-f]{64}$"#, options: .regularExpression) != nil {
                    original = URL(fileURLWithPath: path); originalSHA = value
                }
                let entry = Entry(pending: pending, name: name, bytes: bytes, sha: hash(bytes), source: original,
                                  sourceSHA: originalSHA, old: old, critical: critical, date: date)
                result.entries.append(entry); total += bytes.count
                if !old { index.append(["pending": pending.lastPathComponent, "source": file.path, "source_sha256": sourceSHA, "sha256": entry.sha, "name": name]) }
            } catch { result.warnings.append("\(file.lastPathComponent): \(error.localizedDescription) The original is retained.") }
        }
        let metadata = try JSONSerialization.data(withJSONObject: index, options: [.sortedKeys])
        _ = try SavedReports.create(metadata, in: session, name: "queue-index.json")
        return result
    }
    static func select(_ entries: [Entry], limit: Int = 16) -> [Entry] {
        guard limit > 0 else { return [] }
        let old = entries.filter(\.old).sorted { $0.date < $1.date }
        let fresh = entries.filter { !$0.old }.sorted { $0.critical && !$1.critical }
        var chosen = Array(fresh.prefix((limit + 1) / 2)) + Array(old.prefix(limit / 2))
        var paths = Set(chosen.map(\.pending))
        for entry in fresh + old where chosen.count < limit && paths.insert(entry.pending).inserted { chosen.append(entry) }
        return chosen
    }
    static func confirm(_ entry: Entry, id: String, in session: URL) throws {
        guard !id.isEmpty, hash(try read(entry.pending, privateFile: true)) == entry.sha else {
            throw SavedReports.Failure("The pending report changed before confirmation.")
        }
        let receipt = try JSONSerialization.data(withJSONObject: ["id": id, "sha256": entry.sha, "name": entry.name], options: [.sortedKeys])
        _ = try SavedReports.create(receipt, in: session, name: "confirmed-" + UUID().uuidString + ".json")
        if let sha = entry.sourceSHA {
            let markers = try sentMarkers(), name = "confirmed-source-" + sha + ".json"
            let sourceReceipt = try JSONSerialization.data(withJSONObject: ["id": id, "sha256": entry.sha, "source_sha256": sha], options: [.sortedKeys])
            if !FileManager.default.fileExists(atPath: markers.appendingPathComponent(name).path) { _ = try SavedReports.create(sourceReceipt, in: markers, name: name) }
        }
        try FileManager.default.moveItem(at: entry.pending, to: entry.pending.deletingLastPathComponent().appendingPathComponent("confirmed-" + entry.pending.lastPathComponent))
    }
}
