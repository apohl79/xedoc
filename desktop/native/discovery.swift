import Foundation
import Network

struct DiscoveredService: Encodable {
    let hostname: String
    let origin: String
}

struct DiscoveryMessage: Encodable {
    let type: String
    var services: [DiscoveredService]?
    var message: String?
}

let encoder = JSONEncoder()
encoder.outputFormatting = [.withoutEscapingSlashes]

func emit(_ message: DiscoveryMessage) {
    guard let data = try? encoder.encode(message), data.count <= 65_536 else { return }
    FileHandle.standardOutput.write(data)
    FileHandle.standardOutput.write(Data([10]))
}

DispatchQueue.global(qos: .utility).async {
    while !FileHandle.standardInput.readData(ofLength: 1).isEmpty {}
    exit(0)
}

let browser = NWBrowser(for: .bonjourWithTXTRecord(type: "_xedoc-gui._tcp", domain: "local."),
                        using: .tcp)
browser.browseResultsChangedHandler = { results, _ in
    var services: [DiscoveredService] = []
    var size = 128
    for result in results {
        guard services.count < 64 else { break }
        guard case .bonjour(let txt) = result.metadata, txt["version"] == "1",
              let origin = txt["origin"], !origin.isEmpty, origin.utf8.count <= 512,
              origin.unicodeScalars.allSatisfy({ (33...126).contains($0.value) }),
              let hostname = txt["hostname"], !hostname.isEmpty, hostname.utf8.count <= 255
        else { continue }
        let service = DiscoveredService(hostname: hostname, origin: origin)
        guard let encoded = try? encoder.encode(service), size + encoded.count + 1 <= 65_000 else { continue }
        size += encoded.count + 1
        services.append(service)
    }
    emit(DiscoveryMessage(type: "hosts", services: services))
}
browser.stateUpdateHandler = { state in
    switch state {
    case .ready:
        emit(DiscoveryMessage(type: "ready"))
    case .waiting(let error):
        emit(DiscoveryMessage(type: "waiting", message: String(error.localizedDescription.prefix(512))))
    case .failed(let error):
        emit(DiscoveryMessage(type: "error", message: String(error.localizedDescription.prefix(512))))
    default:
        break
    }
}
browser.start(queue: .main)
dispatchMain()
