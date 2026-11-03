import Combine
import Foundation
import Network

struct DiscoveredHost: Identifiable {
    let hostname: String
    let url: URL
    var id: String { url.absoluteString }

    static func validatedURL(_ origin: String) -> URL? {
        guard origin.count <= 512,
              origin.unicodeScalars.allSatisfy({ (33...126).contains($0.value) }),
              let components = URLComponents(string: origin),
              components.scheme == "https",
              let host = components.host, !host.isEmpty,
              components.user == nil, components.password == nil,
              components.query == nil, components.fragment == nil,
              components.path.isEmpty || components.path == "/",
              components.port.map({ (1...65535).contains($0) }) ?? true
        else { return nil }
        return components.url
    }
}

@MainActor
final class HostDiscovery: ObservableObject {
    @Published private(set) var hosts: [DiscoveredHost] = []
    @Published private(set) var scanning = false
    @Published private(set) var status = "Searching local network…"
    private var browser: NWBrowser?
    private var scanTask: Task<Void, Never>?
    private var generation = UUID()

    func stop() {
        generation = UUID()
        scanTask?.cancel()
        scanTask = nil
        browser?.cancel()
        browser = nil
        scanning = false
    }

    func scan() {
        stop()
        hosts = []
        scanning = true
        status = "Searching local network…"
        let current = generation
        let browser = NWBrowser(for: .bonjourWithTXTRecord(type: "_xedoc-gui._tcp", domain: "local."),
                                using: .tcp)
        self.browser = browser
        browser.browseResultsChangedHandler = { [weak self] results, _ in
            Task { @MainActor in
                guard let self, self.generation == current else { return }
                var discovered: [DiscoveredHost] = []
                for result in results {
                    guard case .bonjour(let txt) = result.metadata, txt["version"] == "1",
                          let origin = txt["origin"], let url = DiscoveredHost.validatedURL(origin),
                          let hostname = txt["hostname"], !hostname.isEmpty, hostname.count <= 255,
                          discovered.count < 64, !discovered.contains(where: { $0.url == url })
                    else { continue }
                    discovered.append(DiscoveredHost(hostname: hostname, url: url))
                }
                self.hosts = discovered.sorted {
                    $0.hostname.localizedStandardCompare($1.hostname) == .orderedAscending
                }
                if !self.scanning { self.status = self.hosts.isEmpty ? "No hosts found" : "Hosts available" }
            }
        }
        browser.stateUpdateHandler = { [weak self] state in
            Task { @MainActor in
                guard let self, self.generation == current else { return }
                switch state {
                case .ready:
                    self.scanning = true
                    self.status = "Searching local network…"
                    self.scanTask?.cancel()
                    self.scanTask = Task { @MainActor [weak self] in
                        do { try await Task.sleep(for: .seconds(5)) }
                        catch { return }
                        guard let self, self.generation == current else { return }
                        self.scanning = false
                        self.status = self.hosts.isEmpty ? "No hosts found. Check Local Network access or enter a host URL." : "Hosts available"
                    }
                case .waiting(let error), .failed(let error):
                    self.scanTask?.cancel()
                    self.scanning = false
                    self.status = "Discovery unavailable: \(error.localizedDescription). Check Local Network access in Settings."
                    if case .failed = state { self.stop() }
                default:
                    break
                }
            }
        }
        browser.start(queue: .main)
    }
}
