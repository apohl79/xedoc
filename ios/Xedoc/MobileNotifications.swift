import UserNotifications

final class MobileNotifications: NSObject, UNUserNotificationCenterDelegate {
    static let shared = MobileNotifications()
    private let center = UNUserNotificationCenter.current()

    private override init() {
        super.init()
        center.delegate = self
    }

    func requestPermission() async {
        guard await center.notificationSettings().authorizationStatus == .notDetermined else { return }
        do {
            _ = try await center.requestAuthorization(options: [.alert, .sound])
        } catch {
            NSLog("Xedoc notification permission failed: %@", error.localizedDescription)
        }
    }

    func receive(_ body: Any, host: URL) {
        guard let event = body as? [String: Any],
              let kind = event["kind"] as? String,
              ["turnFinished", "inputNeeded"].contains(kind),
              let thread = event["threadId"] as? String, !thread.isEmpty, thread.count <= 128,
              let project = event["projectName"] as? String, project.count <= 1024
        else { return }
        let content = UNMutableNotificationContent()
        content.title = kind == "inputNeeded" ? "Xedoc needs your input" : "Xedoc turn finished"
        content.body = String(project.prefix(120))
        content.subtitle = host.host ?? "Xedoc"
        content.sound = .default
        let identifier = "\(host.absoluteString):\(thread):\(kind)"
        Task {
            guard [.authorized, .provisional, .ephemeral].contains(
                await center.notificationSettings().authorizationStatus
            ) else { return }
            do {
                try await center.add(UNNotificationRequest(identifier: identifier, content: content, trigger: nil))
            } catch {
                NSLog("Xedoc notification delivery failed: %@", error.localizedDescription)
            }
        }
    }

    func userNotificationCenter(_ center: UNUserNotificationCenter,
                                willPresent notification: UNNotification,
                                withCompletionHandler completionHandler: @escaping (UNNotificationPresentationOptions) -> Void) {
        completionHandler([.banner, .sound, .list])
    }
}
