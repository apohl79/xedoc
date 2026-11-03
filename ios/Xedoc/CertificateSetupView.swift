import SwiftUI

struct CertificateSetupView: View {
    let host: DiscoveredHost
    let retry: () -> Void
    @Environment(\.dismiss) private var dismiss
    @Environment(\.openURL) private var openURL
    @State private var safariUnavailable = false

    var body: some View {
        NavigationStack {
            List {
                Section("1. Verify and download") {
                    Text(host.url.absoluteString)
                        .font(.caption)
                        .foregroundStyle(.secondary)
                        .textSelection(.enabled)
                    Text("Open this host’s setup page in Safari. Compare its CA SHA-256 fingerprint with the host administrator through a separate trusted channel before downloading the Apple installation profile.")
                    Button("Open Certificate Setup in Safari", systemImage: "safari") {
                        openURL(host.url.appendingPathComponent("trust")) { accepted in
                            safariUnavailable = !accepted
                        }
                    }
                    Text("Safari may show a privacy warning. Do not continue unless you can independently verify the host and CA. If Safari cannot open the page, obtain the public CA profile and fingerprint directly from the administrator.")
                        .font(.footnote)
                        .foregroundStyle(.secondary)
                }
                Section("2. Install the profile") {
                    Text("After downloading, open Settings → Profile Downloaded → Install. If it is not shown, look in General → VPN & Device Management.")
                }
                Section("3. Enable full trust") {
                    Text("In Settings → General → About → Certificate Trust Settings, enable full trust for the verified Xedoc CA.")
                    Text("Only trust a CA whose administrator you trust. It can issue certificates for websites. Xedoc cannot install or enable this trust for you.")
                        .font(.footnote)
                        .foregroundStyle(.secondary)
                }
            }
            .navigationTitle("Certificate Setup")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("Done") { dismiss() }
                }
                ToolbarItem(placement: .confirmationAction) {
                    Button("Retry", systemImage: "arrow.clockwise", action: retry)
                }
            }
            .alert("Could Not Open Safari", isPresented: $safariUnavailable) {
                Button("OK", role: .cancel) {}
            } message: {
                Text("Open \(host.url.appendingPathComponent("trust").absoluteString) in Safari.")
            }
        }
    }
}
