import Combine
import Foundation
import Network

enum HostRoute: String, Codable {
    case direct
    case bridge

    var label: String { self == .bridge ? "Bridge" : "Direct" }
}

struct DiscoveredHost: Identifiable {
    let hostname: String
    let url: URL
    var route: HostRoute = .direct
    var id: String { url.absoluteString }

    var displayName: String { Self.displayName(for: hostname) }

    static func displayName(for hostname: String) -> String {
        hostname.replacingOccurrences(
            of: "^([^.]+)-[0-9]+(?=\\.|$)", with: "$1", options: .regularExpression)
    }

    static func validatedURL(_ origin: String) -> URL? {
        guard origin.count <= 512,
              origin.unicodeScalars.allSatisfy({ (33...126).contains($0.value) }),
              let components = URLComponents(string: origin),
              let host = components.host, !host.isEmpty,
              components.user == nil, components.password == nil,
              components.query == nil, components.fragment == nil,
              components.path.isEmpty || components.path == "/",
              components.port.map({ (1...65535).contains($0) }) ?? true
        else { return nil }
        if components.scheme != "https" {
            #if os(macOS)
            guard components.scheme == "http", ["127.0.0.1", "localhost", "[::1]"].contains(host.lowercased())
            else { return nil }
            #else
            return nil
            #endif
        }
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
    private var pairedTask: Task<Void, Never>?
    private var nearbyHosts: [DiscoveredHost] = []
    private var pairedHosts: [DiscoveredHost] = []
    private var generation = UUID()

    func stop() {
        generation = UUID()
        scanTask?.cancel()
        scanTask = nil
        pairedTask?.cancel()
        pairedTask = nil
        browser?.cancel()
        browser = nil
        scanning = false
    }

    func scan() {
        stop()
        hosts = []
        nearbyHosts = []
        pairedHosts = []
        scanning = true
        status = "Searching nearby and paired hosts…"
        let current = generation
        pairedTask = Task { [weak self] in
            while !Task.isCancelled {
                await withTaskGroup(of: (String, DiscoveredHost?).self) { group in
                    var pending = HostDevicePairing.hosts.makeIterator()
                    for _ in 0..<4 {
                        if let host = pending.next() {
                            group.addTask { (host.id, await HostDevicePairing.availableHost(host)) }
                        }
                    }
                    while let (id, host) = await group.next() {
                        guard let self, self.generation == current, !Task.isCancelled else {
                            group.cancelAll()
                            return
                        }
                        self.pairedHosts.removeAll { $0.id == id }
                        if let host { self.pairedHosts.append(host) }
                        self.updateHosts()
                        if let next = pending.next() {
                            group.addTask { (next.id, await HostDevicePairing.availableHost(next)) }
                        }
                    }
                }
                do { try await Task.sleep(for: .seconds(15)) }
                catch { return }
            }
        }
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
                self.nearbyHosts = discovered
                self.updateHosts()
                if !self.scanning { self.status = self.hosts.isEmpty ? "No hosts found" : "Hosts available" }
            }
        }
        browser.stateUpdateHandler = { [weak self] state in
            Task { @MainActor in
                guard let self, self.generation == current else { return }
                switch state {
                case .ready:
                    self.scanning = true
                    self.status = "Searching nearby and paired hosts…"
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
                    if case .failed = state { self.browser?.cancel(); self.browser = nil }
                default:
                    break
                }
            }
        }
        browser.start(queue: .main)
    }

    private func updateHosts() {
        hosts = (nearbyHosts + pairedHosts.filter { paired in
            !nearbyHosts.contains(where: { $0.id == paired.id })
        }).sorted {
            $0.hostname.localizedStandardCompare($1.hostname) == .orderedAscending
        }
        if !scanning { status = hosts.isEmpty ? "No hosts found" : "Hosts available" }
    }
}
