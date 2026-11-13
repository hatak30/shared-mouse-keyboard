import Foundation
import UIKit

struct Me: Decodable {
    var nodeId: String
    var name: String
    var os: String
    var port: Int
    var fingerprint: String
}

struct Pairing: Decodable {
    var kind: String
    var code: String?
    var error: String?
}

struct Peer: Decodable, Identifiable {
    var nodeId: String
    var name: String
    var os: String?
    var addr: String?
    var connected: Bool
    var trusted: Bool
    var pairing: Pairing?
    var id: String { nodeId }
}

struct LogEntry: Decodable {
    var text: String
}

struct UiState: Decodable {
    var me: Me
    var peers: [Peer]
    var manualPeers: [String]
    var controlling: String?
    var controlledBy: String?
    var log: [LogEntry]
}

func osLabel(_ os: String?) -> String {
    switch os {
    case "MacOs": return "macOS"
    case "Windows": return "Windows"
    case "Linux": return "Linux"
    case "Android": return "Android"
    case "Ios": return "iOS"
    default: return "Unknown system"
    }
}

final class Model: ObservableObject {
    @Published var state: UiState?
    @Published var startError: String?

    private let decoder: JSONDecoder = {
        let d = JSONDecoder()
        d.keyDecodingStrategy = .convertFromSnakeCase
        return d
    }()

    func start() {
        let bounds = UIScreen.main.bounds
        smk_set_screen(UInt32(bounds.width), UInt32(bounds.height))
        smk_set_callback({ context in
            guard let context else { return }
            Unmanaged<Model>.fromOpaque(context).takeUnretainedValue().refresh()
        }, Unmanaged.passUnretained(self).toOpaque())

        let fm = FileManager.default
        let support = fm.urls(for: .applicationSupportDirectory, in: .userDomainMask)[0].appendingPathComponent("config")
        let downloads = fm.urls(for: .documentDirectory, in: .userDomainMask)[0].appendingPathComponent("Downloads")
        try? fm.createDirectory(at: support, withIntermediateDirectories: true)
        try? fm.createDirectory(at: downloads, withIntermediateDirectories: true)

        DispatchQueue.global().async {
            let error = Self.take(smk_start(support.path, downloads.path))
            DispatchQueue.main.async {
                if !error.isEmpty {
                    self.startError = error
                    return
                }
                self.refresh()
                if !UserDefaults.standard.bool(forKey: "named") {
                    UserDefaults.standard.set(true, forKey: "named")
                    self.send("set_name", UIDevice.current.name)
                }
            }
        }
    }

    func refresh() {
        let json = Self.take(smk_state())
        guard let data = json.data(using: .utf8), !data.isEmpty else { return }
        do {
            let decoded = try decoder.decode(UiState.self, from: data)
            DispatchQueue.main.async { self.state = decoded }
        } catch {
            DispatchQueue.main.async { self.startError = "Cannot read engine state: \(error)" }
        }
    }

    func send(_ name: String, _ a: String = "", _ b: String = "") {
        let error = Self.take(smk_command(name, a, b))
        if !error.isEmpty { NSLog("smk %@: %@", name, error) }
    }

    func input(_ kind: Int32, _ a: Double, _ b: Double) {
        smk_input(kind, a, b)
    }

    private static func take(_ p: UnsafeMutablePointer<CChar>?) -> String {
        guard let p else { return "" }
        defer { smk_free_string(p) }
        return String(cString: p)
    }
}
