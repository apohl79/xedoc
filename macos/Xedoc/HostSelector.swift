import SwiftUI

struct HostSelector: View {
    let select: (DiscoveredHost) -> Void
    @StateObject private var discovery = HostDiscovery()
    @State private var address = ""
    @State private var invalidAddress = false
    @State private var pairing = false
    @State private var localHost = Self.readLocalHost()

    var body: some View {
        VStack(spacing: 0) {
            HStack {
                VStack(alignment: .leading, spacing: 4) {
                    Text("Hosts")
                        .font(.system(size: 20, weight: .semibold))
                        .foregroundStyle(HostSelectorPalette.primary)
                    Text("Connect to a Xedoc host")
                        .font(.system(size: 13))
                        .foregroundStyle(HostSelectorPalette.secondary)
                }
                Spacer()
                Button("Pair a host") { pairing = true }
                    .buttonStyle(.bordered)
                Button { localHost = Self.readLocalHost(); discovery.scan() } label: {
                    Image(systemName: "arrow.clockwise")
                }
                .buttonStyle(.bordered)
                .tint(HostSelectorPalette.accent)
                .help("Refresh hosts")
                .accessibilityLabel("Refresh hosts")
            }
            .padding(.leading, 22)
            .padding(.trailing, 22)
            .padding(.top, 20)
            .padding(.bottom, 18)

            ScrollView {
                VStack(alignment: .leading, spacing: 10) {
                    if let localHost {
                        Text("THIS MAC")
                            .font(.system(size: 11, weight: .semibold))
                            .foregroundStyle(HostSelectorPalette.secondary)
                        hostRow(localHost)
                    }
                    Text("AVAILABLE HOSTS")
                        .font(.system(size: 11, weight: .semibold))
                        .tracking(0.8)
                        .foregroundStyle(HostSelectorPalette.secondary)

                    if discovery.hosts.isEmpty {
                        discoveryStatus
                    } else {
                        ForEach(discovery.hosts) { host in
                            hostRow(host)
                        }
                        discoveryStatus
                    }
                }
                .padding(.horizontal, 22)
                .padding(.bottom, 20)
                .frame(maxWidth: .infinity, alignment: .leading)
            }

            VStack(alignment: .leading, spacing: 12) {
                Text("Connect to a host")
                    .font(.system(size: 13, weight: .semibold))
                    .foregroundStyle(HostSelectorPalette.primary)

                HStack(spacing: 10) {
                    TextField("https://hostname:port", text: $address)
                        .font(.system(size: 13, design: .monospaced))
                        .textFieldStyle(.plain)
                        .padding(.horizontal, 11)
                        .padding(.vertical, 9)
                        .background(HostSelectorPalette.surface, in: RoundedRectangle(cornerRadius: 6))
                        .overlay {
                            RoundedRectangle(cornerRadius: 6)
                                .stroke(HostSelectorPalette.border, lineWidth: 1)
                        }
                        .onSubmit(openAddress)
                        .accessibilityLabel("Host URL")

                    Button("Connect", action: openAddress)
                        .buttonStyle(.borderedProminent)
                        .tint(HostSelectorPalette.accent)
                        .disabled(address.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
                }
            }
            .padding(22)
            .frame(maxWidth: .infinity, alignment: .leading)
            .background(HostSelectorPalette.footer)
            .overlay(alignment: .top) {
                Rectangle().fill(HostSelectorPalette.border).frame(height: 1)
            }
        }
        .frame(minWidth: 500, minHeight: 380)
        .background(HostSelectorPalette.background)
        .preferredColorScheme(.dark)
        .task { discovery.scan() }
        .onDisappear { discovery.stop() }
        .sheet(isPresented: $pairing, onDismiss: { discovery.scan() }) {
            HostPairingView { url in select(DiscoveredHost(hostname: url.host ?? "Host", url: url)) }
        }
        .alert("Enter an HTTPS host URL, or HTTP for localhost", isPresented: $invalidAddress) {
            Button("OK", role: .cancel) {}
        }
    }

    static func readLocalHost() -> DiscoveredHost? {
        guard let access = localAccess() else { return nil }
        return DiscoveredHost(hostname: "This Mac — no setup required", url: access.url)
    }

    static func localAccess() -> (url: URL, token: String)? {
        let home = ProcessInfo.processInfo.environment["XEDOC_HOME"].map { URL(fileURLWithPath: $0) }
            ?? FileManager.default.homeDirectoryForCurrentUser.appendingPathComponent(".xedoc")
        let path = home.appendingPathComponent("host-gui/local.json")
        guard let data = try? Data(contentsOf: path), data.count <= 1024,
              let value = try? JSONSerialization.jsonObject(with: data) as? [String: String],
              let origin = value["origin"], let url = DiscoveredHost.validatedURL(origin),
              url.scheme == "http", url.host == "127.0.0.1",
              let token = value["token"], token.count == 64,
              token.utf8.allSatisfy({ (45...122).contains($0) })
        else { return nil }
        return (url, token)
    }

    private var discoveryStatus: some View {
        HStack(spacing: 11) {
            if discovery.scanning {
                ProgressView().controlSize(.small).tint(HostSelectorPalette.accent)
                    .frame(width: 34, height: 34)
                    .background(HostSelectorPalette.iconBackground, in: RoundedRectangle(cornerRadius: 6))
            } else {
                Image(systemName: discovery.hosts.isEmpty ? "dot.radiowaves.left.and.right" : "checkmark.circle")
                    .font(.system(size: 16, weight: .medium))
                    .foregroundStyle(HostSelectorPalette.accent)
                    .frame(width: 34, height: 34)
                    .background(HostSelectorPalette.iconBackground, in: RoundedRectangle(cornerRadius: 6))
            }

            VStack(alignment: .leading, spacing: 3) {
                Text(discovery.scanning ? "Searching for hosts" : (discovery.hosts.isEmpty ? "No hosts found" : "Discovery complete"))
                    .font(.system(size: 13, weight: .medium))
                    .foregroundStyle(HostSelectorPalette.primary)
                Text(discovery.scanning ? "Xedoc is scanning your local network." : discovery.status)
                    .font(.system(size: 12))
                    .foregroundStyle(HostSelectorPalette.secondary)
                    .fixedSize(horizontal: false, vertical: true)
            }
            Spacer(minLength: 0)
        }
        .padding(12)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(HostSelectorPalette.surface, in: RoundedRectangle(cornerRadius: 8))
        .overlay {
            RoundedRectangle(cornerRadius: 8)
                .stroke(HostSelectorPalette.border, lineWidth: 1)
        }
    }

    private func hostRow(_ host: DiscoveredHost) -> some View {
        Button { select(host) } label: {
            HStack(spacing: 11) {
                Image(systemName: "desktopcomputer")
                    .font(.system(size: 17, weight: .medium))
                    .foregroundStyle(HostSelectorPalette.accent)
                    .frame(width: 34, height: 34)
                    .background(HostSelectorPalette.iconBackground, in: RoundedRectangle(cornerRadius: 6))

                VStack(alignment: .leading, spacing: 3) {
                    Text(host.hostname)
                        .font(.system(size: 13, weight: .medium))
                        .foregroundStyle(HostSelectorPalette.primary)
                        .lineLimit(1)
                    Text(host.url.absoluteString)
                        .font(.system(size: 11, design: .monospaced))
                        .foregroundStyle(HostSelectorPalette.secondary)
                        .lineLimit(1)
                        .truncationMode(.middle)
                }
                Spacer(minLength: 8)
                Text(host.route.label)
                    .font(.system(size: 11, weight: .medium))
                    .foregroundStyle(HostSelectorPalette.secondary)
                Image(systemName: "chevron.right")
                    .font(.system(size: 11, weight: .semibold))
                    .foregroundStyle(HostSelectorPalette.secondary)
            }
            .padding(12)
            .contentShape(RoundedRectangle(cornerRadius: 8))
        }
        .buttonStyle(.plain)
        .background(HostSelectorPalette.surface, in: RoundedRectangle(cornerRadius: 8))
        .overlay {
            RoundedRectangle(cornerRadius: 8)
                .stroke(HostSelectorPalette.border, lineWidth: 1)
        }
    }

    private func openAddress() {
        guard let url = DiscoveredHost.validatedURL(address.trimmingCharacters(in: .whitespacesAndNewlines)) else {
            invalidAddress = true
            return
        }
        select(DiscoveredHost(hostname: url.host ?? address, url: url))
    }
}

private enum HostSelectorPalette {
    static let background = Color(red: 18.0 / 255, green: 23.0 / 255, blue: 28.0 / 255)
    static let footer = Color(red: 20.0 / 255, green: 25.0 / 255, blue: 30.0 / 255)
    static let surface = Color(red: 27.0 / 255, green: 33.0 / 255, blue: 40.0 / 255)
    static let iconBackground = Color(red: 39.0 / 255, green: 34.0 / 255, blue: 51.0 / 255)
    static let border = Color(red: 44.0 / 255, green: 52.0 / 255, blue: 62.0 / 255)
    static let primary = Color(red: 229.0 / 255, green: 232.0 / 255, blue: 236.0 / 255)
    static let secondary = Color(red: 151.0 / 255, green: 162.0 / 255, blue: 174.0 / 255)
    static let accent = Color(red: 201.0 / 255, green: 138.0 / 255, blue: 255.0 / 255)
}
