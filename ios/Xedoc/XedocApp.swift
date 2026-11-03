import SwiftUI

@main
struct XedocApp: App {
    @State private var selectedHost: DiscoveredHost?

    var body: some Scene {
        WindowGroup {
            NavigationStack {
                if let host = selectedHost {
                    HostScreen(host: host)
                } else {
                    HostSelector { selectedHost = $0 }
                }
            }
            .preferredColorScheme(.dark)
        }
    }
}

private struct HostSelector: View {
    let select: (DiscoveredHost) -> Void
    @StateObject private var discovery = HostDiscovery()
    @Environment(\.scenePhase) private var scenePhase
    @State private var address = ""
    @State private var invalidAddress = false

    var body: some View {
        List {
            Section("Nearby hosts") {
                ForEach(discovery.hosts) { host in
                    Button {
                        select(host)
                    } label: {
                        HStack {
                            Image(systemName: "desktopcomputer")
                            VStack(alignment: .leading) {
                                Text(host.hostname).foregroundStyle(.primary)
                                Text(host.url.absoluteString).font(.caption).foregroundStyle(.secondary)
                            }
                            Spacer()
                            Image(systemName: "chevron.right").foregroundStyle(.secondary)
                        }
                        .padding(.vertical, 6)
                    }
                }
                HStack {
                    if discovery.scanning { ProgressView() }
                    Text(discovery.status).font(.subheadline).foregroundStyle(.secondary)
                }
            }
            Section("Open host") {
                TextField("https://hostname:port", text: $address)
                    .keyboardType(.URL)
                    .textContentType(.URL)
                    .textInputAutocapitalization(.never)
                    .autocorrectionDisabled()
                    .onSubmit(openAddress)
                Button("Connect", action: openAddress)
                    .disabled(address.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
            }
        }
        .navigationTitle("Hosts")
        .toolbar {
            Button("Refresh hosts", systemImage: "arrow.clockwise") { discovery.scan() }
        }
        .refreshable { discovery.scan() }
        .task { discovery.scan() }
        .onDisappear { discovery.stop() }
        .onChange(of: scenePhase) { _, phase in
            if phase == .active { discovery.scan() }
            else { discovery.stop() }
        }
        .alert("Enter an HTTPS host URL", isPresented: $invalidAddress) {
            Button("OK", role: .cancel) {}
        }
    }

    private func openAddress() {
        guard let url = DiscoveredHost.validatedURL(
            address.trimmingCharacters(in: .whitespacesAndNewlines)
        ) else {
            invalidAddress = true
            return
        }
        select(DiscoveredHost(hostname: url.host ?? address, url: url))
    }
}

private struct HostScreen: View {
    let host: DiscoveredHost
    @State private var reload = 0
    @State private var error: HostLoadError?
    @State private var showCertificateSetup = false

    var body: some View {
        HostWebView(url: host.url, reload: reload, error: $error)
            .overlay {
                if let error {
                    VStack(spacing: 16) {
                        Image(systemName: "exclamationmark.triangle").font(.largeTitle)
                        Text(error.message).multilineTextAlignment(.center)
                        if case .certificate = error {
                            Button("Set Up Certificate", systemImage: "checkmark.shield") {
                                showCertificateSetup = true
                            }
                            .buttonStyle(.borderedProminent)
                        }
                        Button("Retry") { self.error = nil; reload += 1 }
                    }
                    .padding(24)
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
                    .background(.background)
                }
            }
            .sheet(isPresented: $showCertificateSetup) {
                CertificateSetupView(host: host) {
                    showCertificateSetup = false
                    error = nil
                    reload += 1
                }
            }
            .toolbar(.hidden, for: .navigationBar)
    }
}
