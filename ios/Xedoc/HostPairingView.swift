import SwiftUI

struct HostPairingView: View {
    let paired: (URL) -> Void
    @Environment(\.dismiss) private var dismiss
    @State private var invitation = ""
    @State private var failure: String?
    @State private var working = false
    @State private var step = 0
    @State private var verified = false
    @State private var pairedURL: URL?

    private var preview: HostInvitation? { try? HostInvitation(invitation) }

    var body: some View {
        VStack(alignment: .leading, spacing: 16) {
            Text("Pair a host").font(.title2.bold())
            Text(pairedURL == nil ? "Step \(step + 1) of 3" : "Pairing complete")
                .font(.subheadline).foregroundStyle(.secondary)
            ScrollView {
                VStack(alignment: .leading, spacing: 16) {
                    if let pairedURL {
                        Label("This device is paired", systemImage: "checkmark.shield")
                            .font(.headline)
                        Text(HostDevicePairing.origin(pairedURL)).textSelection(.enabled)
                        Text("On this device: connect, then sign in with your host account. No certificate profile is needed in Settings.")
                    } else if step == 0 {
                        Text("Hosts in external network mode and hosts reached through the sendnix bridge require pairing. Internal direct connections only need sign-in.")
                        Label("On a trusted Mac connected to the host", systemImage: "desktopcomputer")
                            .font(.headline)
                        Text("In Xedoc, open the host and sign in as an administrator. Choose File → Pair Device… and create an invitation.")
                        Text("If you only have terminal access to the host, run this command there:")
                        Text("xedoc-remote-agentd device-pairing invite")
                            .font(.system(.callout, design: .monospaced)).textSelection(.enabled)
                        Text("For a direct host in external network mode, use host-gui pair-device instead of device-pairing invite.")
                        Text("For first-time setup, use Pair Device on a trusted Mac to check for an unused sendnix bridge port, then run device-pairing setup on the host to install and start its connector. Each public port serves one host; all devices pairing with that host share it.")
                        Label("Then return to this device", systemImage: "iphone.and.arrow.forward")
                            .font(.headline)
                        Text("Transfer the invitation directly using AirDrop or another trusted channel. Never get it from sendnix or the bridge.")
                    } else if step == 1 {
                        Label("On this device: paste the invitation", systemImage: "doc.on.clipboard")
                            .font(.headline)
                        Text("On the trusted Mac, use Copy invitation or Share. Each invitation works once and expires after 10 minutes.")
                        SecureField("Pairing invitation", text: $invitation)
                            .textFieldStyle(.roundedBorder)
                            .accessibilityIdentifier("pairingInvitation")
                            .onChange(of: invitation) { _, _ in failure = nil; verified = false }
                        if !invitation.isEmpty && preview == nil {
                            Text("This is not a valid invitation. Copy the complete invitation from the trusted host.")
                                .font(.callout).foregroundStyle(.red)
                        }
                    } else if let preview {
                        Label("Compare both devices", systemImage: "checkmark.shield")
                            .font(.headline)
                        Text("Check that this host address and certificate fingerprint match those shown on the trusted Mac, or printed beside the invitation in the host terminal.")
                        Text(preview.origin).font(.headline).textSelection(.enabled)
                        Text("Host certificate · SHA-256").font(.caption).foregroundStyle(.secondary)
                        Text(preview.fingerprint).font(.system(.caption, design: .monospaced))
                            .textSelection(.enabled)
                        Toggle("I verified the host address and fingerprint", isOn: $verified)
                        Text("Pair creates a private key in this device’s Keychain. The key is not sent to the host or bridge.")
                            .font(.callout).foregroundStyle(.secondary)
                    }
                    if let failure {
                        Text(failure).foregroundStyle(.red).font(.callout)
                        Text("Check that the host is reachable and, for a bridge connection, its connector is running. If the invitation expired or was already used, create another on the trusted Mac and paste it here.")
                            .font(.callout)
                    }
                }.frame(maxWidth: .infinity, alignment: .leading)
            }
            HStack {
                if pairedURL == nil {
                    Button("Cancel") { invitation = ""; dismiss() }.disabled(working)
                    if step > 0 {
                        Button("Back") { step -= 1; verified = false; failure = nil }.disabled(working)
                    }
                }
                Spacer()
                if let pairedURL {
                    Button("Connect to host") { dismiss(); paired(pairedURL) }
                        .buttonStyle(.borderedProminent)
                } else if step < 2 {
                    Button(step == 0 ? "I have an invitation" : "Verify host") { step += 1 }
                        .buttonStyle(.borderedProminent)
                        .disabled(step == 1 && preview == nil)
                } else {
                    if working { ProgressView().controlSize(.small).accessibilityLabel("Pairing") }
                    Button("Pair this device") {
                        guard let preview else { return }
                        working = true
                        failure = nil
                        Task { @MainActor in
                            do {
                                #if os(macOS)
                                let name = Host.current().localizedName ?? "macOS"
                                #else
                                let name = UIDevice.current.name
                                #endif
                                pairedURL = try await HostDevicePairing.pair(preview, name: name)
                                invitation = ""
                            } catch {
                                failure = error.localizedDescription
                            }
                            working = false
                        }
                    }
                    .buttonStyle(.borderedProminent)
                    .disabled(!verified || working)
                }
            }
        }
        .padding(24)
        .frame(idealWidth: 520, idealHeight: 520)
        .interactiveDismissDisabled(working || pairedURL != nil)
    }
}
