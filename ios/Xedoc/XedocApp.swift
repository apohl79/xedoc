import SwiftUI

@main
struct XedocApp: App {
    @UIApplicationDelegateAdaptor(AppDelegate.self) private var appDelegate
    @State private var selectedHost: DiscoveredHost?
    @State private var selectionError: String?
    @State private var changingHost = false

    var body: some Scene {
        WindowGroup {
            NavigationStack {
                if let host = selectedHost {
                    HostScreen(host: host) { changeHost(nil) }
                } else {
                    HostSelector { changeHost($0) }
                }
            }
            .disabled(changingHost)
            .overlay { if changingHost { ProgressView("Removing old push subscription…").padding().background(.regularMaterial) } }
            .alert("Could not change hosts", isPresented: Binding(
                get: { selectionError != nil }, set: { if !$0 { selectionError = nil } }
            )) { Button("OK", role: .cancel) {} } message: { Text(selectionError ?? "") }
            .preferredColorScheme(.dark)
        }
    }

    private func changeHost(_ host: DiscoveredHost?) {
        guard !changingHost else { return }
        changingHost = true
        Task {
            defer { changingHost = false }
            do {
                try await PushSubscription.shared.select(host?.url)
                selectedHost = host
            } catch {
                selectionError = error.localizedDescription
            }
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
    let disconnect: () -> Void
    @ObservedObject private var push = PushSubscription.shared
    @Environment(\.scenePhase) private var scenePhase
    @State private var reload = 0
    @State private var error: HostLoadError?
    @State private var showCertificateSetup = false

    var body: some View {
        HostWebView(url: host.url, reload: reload, error: $error, disconnect: disconnect)
            .task { await MobileNotifications.shared.requestPermission() }
            .onChange(of: scenePhase) { _, phase in
                if phase == .active { Task { await MobileNotifications.shared.requestPermission() } }
            }
            .safeAreaInset(edge: .bottom) {
                if let notice = push.notice {
                    Text(notice).font(.caption).padding(8).frame(maxWidth: .infinity).background(.regularMaterial)
                }
            }
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
                        Button("Hosts", action: disconnect)
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
