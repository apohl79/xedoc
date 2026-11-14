import SwiftUI
import WebKit

@MainActor
struct PairDeviceView: View {
    let connection: HostConnection
    @Environment(\.dismiss) private var dismiss
    @State private var enabled: Bool?
    @State private var canInvite = false
    @State private var localPairing = LocalDevicePairing()
    @State private var invitation = ""
    @State private var invitationID = ""
    @State private var expiresAt = Date.distantPast
    @State private var status = "pending"
    @State private var failure: String?
    @State private var working = false

    private var preview: HostInvitation? { try? HostInvitation(invitation) }

    var body: some View {
        VStack(alignment: .leading, spacing: 16) {
            Text("Pair another device").font(.title2.bold())
            Text(invitation.isEmpty ? "Step 1 of 3 · On this Mac" : status == "completed"
                 ? "Step 3 of 3 · On the other device" : "Step 2 of 3 · On the other device")
                .font(.subheadline).foregroundStyle(.secondary)
            ScrollView {
                VStack(alignment: .leading, spacing: 16) {
                    Text(preview?.origin ?? HostDevicePairing.origin(connection.url))
                        .font(.headline).textSelection(.enabled)
                    if status == "completed" {
                        Label("The host accepted the device", systemImage: "checkmark.shield")
                            .font(.headline)
                        Text("On the other device: finish saving the pairing, tap Connect to host, then sign in with your host account. No certificate profile is needed in Settings.")
                    } else if let preview {
                        Text("On the iPhone, iPad, or other Mac: open Xedoc → Hosts → Pair a host. Choose I have an invitation, paste the invitation, then choose Verify host.")
                        HStack {
                            Button("Copy invitation", systemImage: "doc.on.doc") {
                                NSPasteboard.general.clearContents()
                                NSPasteboard.general.setString(invitation, forType: .string)
                            }
                            ShareLink("Share", item: invitation)
                        }.disabled(status != "pending")
                        Text("Transfer only through AirDrop or another trusted channel, never through the bridge. Treat the invitation like a password.")
                            .font(.callout).foregroundStyle(.secondary)
                        Text("On both devices: compare this host address and fingerprint. On the other device: confirm the match and choose Pair this device.")
                        Text("Host certificate · SHA-256").font(.caption).foregroundStyle(.secondary)
                        Text(preview.fingerprint).font(.system(.caption, design: .monospaced))
                            .textSelection(.enabled)
                        if status == "pending" {
                            Label("Waiting for the other device…", systemImage: "clock")
                            Text("Single use · Expires \(expiresAt.formatted(date: .omitted, time: .shortened)). Creating another invitation replaces this one.")
                                .font(.callout).foregroundStyle(.secondary)
                        } else {
                            Text(status == "expired" ? "This invitation expired. Create another invitation, then paste and verify it again on the other device."
                                 : "Another invitation replaced this one. Use the newest invitation, or create another here.")
                                .foregroundStyle(.orange)
                        }
                    } else if enabled == true {
                        Text("On this Mac: create a single-use invitation for the connected host. Keep this window open while you pair the other device.")
                        Text("Next, you’ll transfer the invitation and compare the host fingerprint on both devices.")
                    } else if canInvite {
                        Label("Pair your other device", systemImage: "iphone.and.arrow.forward")
                            .font(.headline)
                        Text("On this Mac: choose Create invitation. Xedoc will enable certificate protection for other devices. You do not need to pair this Mac or install a certificate profile.")
                        Text("Local access on the host Mac stays unchanged. Existing unpaired remote connections will be disconnected.")
                            .font(.callout).foregroundStyle(.secondary)
                        Text("Next, open Xedoc on the other device to receive the invitation and verify the host.")
                    } else if enabled == false {
                        Label("Host setup required", systemImage: "network").font(.headline)
                        Text("On the host computer: configure private-CA HTTPS with an address the other device can reach. Then return here and choose Check again. Local access on this Mac does not need pairing.")
                        Text("If HTTPS is already configured, update the host to enable invitation creation from this app.")
                    } else if failure == nil {
                        ProgressView("Checking host protection…")
                    }
                    if let failure {
                        Text(failure).foregroundStyle(.red).font(.callout)
                        Text("For this Mac’s host, connect using This Mac in Hosts. For another host, sign in as an administrator and retry.")
                            .font(.callout)
                    }
                }.frame(maxWidth: .infinity, alignment: .leading)
            }
            HStack {
                Button(status == "completed" ? "Done" : "Close") { dismiss() }.disabled(working)
                Spacer()
                if working { ProgressView().controlSize(.small) }
                if status != "completed" {
                    Button(canInvite ? (invitation.isEmpty ? "Create invitation" : "New invitation") : "Check again") {
                        Task { await update(create: canInvite) }
                    }
                    .buttonStyle(.borderedProminent).disabled(working)
                }
            }
        }
        .padding(24)
        .frame(width: 560, height: 580)
        .interactiveDismissDisabled(working)
        .task { await update(create: false) }
        .task(id: invitationID) {
            guard !invitationID.isEmpty else { return }
            while !Task.isCancelled && status == "pending" {
                do {
                    try await Task.sleep(for: .seconds(2))
                    let result = try await request("device/pairing-status", body: ["invitationId": invitationID])
                    try Task.checkCancellation()
                    guard let next = result["status"] as? String,
                          ["pending", "completed", "expired", "replaced"].contains(next)
                    else { throw pairingError("Unexpected pairing status from the host.") }
                    status = next
                    failure = nil
                } catch {
                    if Task.isCancelled { return }
                    failure = error.localizedDescription
                    if Date() >= expiresAt { status = "expired" }
                }
            }
        }
    }

    private func update(create: Bool) async {
        working = true
        failure = nil
        defer { working = false }
        do {
            let result = try await request(create ? "device/invite" : "device/pairing", body: create ? [:] : nil)
            if create {
                guard let text = result["invitation"] as? String,
                      let identifier = result["invitationId"] as? String,
                      let expiration = result["expiresAt"] as? Double
                else { throw pairingError("Unexpected invitation from the host.") }
                _ = try HostInvitation(text)
                invitation = text
                expiresAt = Date(timeIntervalSince1970: expiration)
                status = "pending"
                invitationID = identifier
                enabled = true
            } else {
                guard let protection = result["enabled"] as? Bool else {
                    throw pairingError("Update the host to a version that supports device pairing.")
                }
                enabled = protection
                canInvite = result["canInvite"] as? Bool ?? protection
            }
        } catch {
            failure = error.localizedDescription
        }
    }

    private func request(_ path: String, body: [String: String]?) async throws -> [String: Any] {
        if let result = try await localPairing.request(for: connection.url, path: path, body: body) {
            return result
        }
        let encoded = try body.map { String(decoding: try JSONEncoder().encode($0), as: UTF8.self) }
        let value = try await connection.webView.callAsyncJavaScript("""
            if (location.origin !== origin) throw new Error("Open and sign in to the host first.");
            const response = await fetch("/api/" + path, {
              method: body === null ? "GET" : "POST",
              credentials: "same-origin", redirect: "error",
              headers: body === null ? {} : {"Content-Type": "application/json"},
              body: body === null ? undefined : body,
              signal: AbortSignal.timeout(15000)
            });
            const text = await response.text();
            if (text.length > 16384) throw new Error("Pairing response is too large.");
            if (!response.ok) {
              let message;
              try { message = JSON.parse(text).error; } catch {}
              return JSON.stringify({error: message || "Sign in as an administrator and update the host if needed."});
            }
            return text;
            """, arguments: ["origin": HostDevicePairing.origin(connection.url), "path": path,
                             "body": encoded as Any? ?? NSNull()], in: nil, contentWorld: .page)
        guard let text = value as? String,
              let result = try JSONSerialization.jsonObject(with: Data(text.utf8)) as? [String: Any]
        else { throw pairingError("Unexpected response. Update the host if needed.") }
        if let message = result["error"] as? String { throw pairingError(String(message.prefix(1024))) }
        return result
    }
}
