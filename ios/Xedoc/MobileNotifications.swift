import UIKit
import UserNotifications

@MainActor
final class MobileNotifications: NSObject, UNUserNotificationCenterDelegate {
    static let shared = MobileNotifications()
    private let center = UNUserNotificationCenter.current()

    private override init() {
        super.init()
        center.delegate = self
    }

    func requestPermission() async {
        if await center.notificationSettings().authorizationStatus == .notDetermined {
            do {
                _ = try await center.requestAuthorization(options: [.alert, .sound])
            } catch {
                PushSubscription.shared.notice = "Notification permission failed. Check iOS Settings."
            }
        }
        await PushSubscription.shared.permissionChanged()
    }

    func receive(_ body: Any, host: URL) {
        guard !PushSubscription.shared.usesRemotePush, PushSubscription.shared.isSelected(host),
              let event = body as? [String: Any],
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
        content.userInfo = PushSubscription.shared.notificationInfo
        let identifier = "\(host.absoluteString):\(thread):\(kind)"
        Task {
            guard [.authorized, .provisional, .ephemeral].contains(
                await center.notificationSettings().authorizationStatus
            ), !PushSubscription.shared.usesRemotePush, PushSubscription.shared.isSelected(host) else { return }
            do {
                try await center.add(UNNotificationRequest(identifier: identifier, content: content, trigger: nil))
            } catch {
                NSLog("Xedoc notification delivery failed: %@", error.localizedDescription)
            }
        }
    }

    nonisolated func userNotificationCenter(_ center: UNUserNotificationCenter,
                                willPresent notification: UNNotification,
                                withCompletionHandler completionHandler: @escaping (UNNotificationPresentationOptions) -> Void) {
        Task { @MainActor in
            let info = notification.request.content.userInfo
            let local = !(notification.request.trigger is UNPushNotificationTrigger)
            let allowed = PushSubscription.shared.matches(info)
                && (!local || !PushSubscription.shared.usesRemotePush)
            completionHandler(allowed ? [.banner, .sound, .list] : [])
        }
    }
}

@MainActor
final class AppDelegate: NSObject, UIApplicationDelegate {
    func application(_ application: UIApplication,
                     didFinishLaunchingWithOptions launchOptions: [UIApplication.LaunchOptionsKey: Any]? = nil) -> Bool {
        _ = MobileNotifications.shared
        application.registerForRemoteNotifications()
        Task { await PushSubscription.shared.permissionChanged() }
        return true
    }

    func application(_ application: UIApplication, didRegisterForRemoteNotificationsWithDeviceToken deviceToken: Data) {
        PushSubscription.shared.receivedToken(deviceToken)
    }

    func application(_ application: UIApplication, didFailToRegisterForRemoteNotificationsWithError error: Error) {
        NSLog("Xedoc APNs device registration failed")
        PushSubscription.shared.registrationFailed()
    }
}
