import AppKit
import UserNotifications

struct SessionRoute {
    let host: URL
    let username: String
    let workspaceId: String
    let projectName: String
    let threadId: String

    init?(info: [AnyHashable: Any]) {
        guard let origin = info["host"] as? String, let host = DiscoveredHost.validatedURL(origin),
              let username = info["username"] as? String, !username.isEmpty, username.count <= 64,
              let workspace = info["workspaceId"] as? String, !workspace.isEmpty, workspace.count <= 1024,
              let project = info["projectName"] as? String, !project.isEmpty, project.count <= 1024,
              let thread = info["threadId"] as? String, !thread.isEmpty, thread.count <= 128
        else { return nil }
        self.host = host
        self.username = username
        workspaceId = workspace
        projectName = project
        threadId = thread
    }

    var info: [String: String] {
        ["host": host.absoluteString, "username": username, "workspaceId": workspaceId,
         "projectName": projectName, "threadId": threadId]
    }
}

@MainActor
final class DesktopNotifications: NSObject, UNUserNotificationCenterDelegate {
    static let shared = DesktopNotifications()
    var openSession: ((SessionRoute) -> Void)?
    private let center = UNUserNotificationCenter.current()

    private override init() {
        super.init()
        center.delegate = self
    }

    func requestPermission() {
        Task {
            do {
                if await center.notificationSettings().authorizationStatus == .notDetermined {
                    _ = try await center.requestAuthorization(options: [.alert, .sound])
                }
            } catch { NSLog("Xedoc notification permission failed: %@", error.localizedDescription) }
        }
    }

    func receive(_ event: [String: Any], host: URL, username: String?) {
        guard let kind = event["kind"] as? String, ["inputNeeded", "turnFinished"].contains(kind),
              let username, event["username"] as? String == username,
              let route = SessionRoute(info: event.merging(["host": host.absoluteString]) { _, new in new })
        else { return }
        let content = UNMutableNotificationContent()
        content.title = kind == "inputNeeded" ? "Xedoc needs your input" : "Xedoc turn finished"
        content.subtitle = DiscoveredHost.displayName(for: host.host ?? "Xedoc")
        content.body = String(route.projectName.prefix(120))
        content.sound = .default
        content.userInfo = route.info
        Task {
            do {
                try await center.add(UNNotificationRequest(
                    identifier: "\(host.absoluteString):\(username):\(route.threadId):\(kind)",
                    content: content, trigger: nil))
            } catch { NSLog("Xedoc notification delivery failed: %@", error.localizedDescription) }
        }
    }

    nonisolated func userNotificationCenter(_ center: UNUserNotificationCenter,
                                           willPresent notification: UNNotification,
                                           withCompletionHandler completionHandler: @escaping (UNNotificationPresentationOptions) -> Void) {
        completionHandler([.banner, .sound, .list])
    }

    nonisolated func userNotificationCenter(_ center: UNUserNotificationCenter,
                                           didReceive response: UNNotificationResponse,
                                           withCompletionHandler completionHandler: @escaping () -> Void) {
        let info = response.notification.request.content.userInfo
        Task { @MainActor in
            if response.actionIdentifier == UNNotificationDefaultActionIdentifier,
               let route = SessionRoute(info: info) {
                NSApp.activate(ignoringOtherApps: true)
                openSession?(route)
            }
            completionHandler()
        }
    }
}
