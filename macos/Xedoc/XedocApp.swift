import AppKit
import SwiftUI

@main
struct XedocApp: App {
    @NSApplicationDelegateAdaptor(DesktopAppDelegate.self) private var delegate

    var body: some Scene {
        Settings { EmptyView() }
            .commands {
                CommandGroup(replacing: .newItem) {
                    Button("Hosts…") { delegate.showHosts() }.keyboardShortcut("n")
                }
                CommandGroup(after: .newItem) {
                    Button("Reload Host") { delegate.reloadHost() }.keyboardShortcut("r")
                }
            }
    }
}

@MainActor
final class DesktopAppDelegate: NSObject, NSApplicationDelegate, NSWindowDelegate {
    private var selector: NSWindow?
    private var connections: [String: HostConnection] = [:]
    private let defaults = UserDefaults.standard
    private var isTerminating = false

    func applicationDidFinishLaunching(_ notification: Notification) {
        DesktopNotifications.shared.openSession = { [weak self] route in
            self?.openHost(route.host, route: route)
        }
        let remembered = (defaults.stringArray(forKey: "connectedHosts") ?? [])
            .compactMap(DiscoveredHost.validatedURL)
        if remembered.isEmpty { showHosts() }
        for url in remembered { openHost(url, restoring: true) }
        Task {
            try? await Task.sleep(for: .seconds(8))
            if !NSApp.windows.contains(where: \.isVisible) { showHosts() }
        }
    }

    func applicationShouldTerminate(_ sender: NSApplication) -> NSApplication.TerminateReply {
        isTerminating = true
        return .terminateNow
    }

    func applicationShouldHandleReopen(_ sender: NSApplication, hasVisibleWindows flag: Bool) -> Bool {
        if !flag { showHosts() }
        return true
    }

    func showHosts() {
        if let selector {
            selector.makeKeyAndOrderFront(nil)
            return
        }
        let window = makeWindow(title: "Hosts", size: NSSize(width: 520, height: 480))
        window.isMovableByWindowBackground = true
        window.contentView = NSHostingView(rootView: HostSelector { [weak self] host in
            self?.openHost(host.url)
        }.preferredColorScheme(.dark))
        selector = window
        window.makeKeyAndOrderFront(nil)
    }

    func openHost(_ url: URL, route: SessionRoute? = nil, restoring: Bool = false) {
        guard let url = DiscoveredHost.validatedURL(url.absoluteString)?.appendingPathComponent("") else { return }
        let key = url.absoluteString
        if let connection = connections[key] {
            connection.window?.makeKeyAndOrderFront(nil)
            if let route { connection.navigate(to: route) }
            return
        }
        let connection = HostConnection(url: url)
        let window = makeWindow(title: url.host ?? "Xedoc", size: NSSize(width: 1200, height: 800))
        if let port = url.port { window.title += ":\(port)" }
        window.minSize = NSSize(width: 720, height: 480)
        connection.window = window
        connection.showHosts = { [weak self] in self?.showHosts() }
        connection.openHost = { [weak self] in self?.openHost($0) }
        connection.authenticationChanged = { [weak self, weak connection] authenticated in
            guard let self, let connection else { return }
            var hosts = defaults.stringArray(forKey: "connectedHosts") ?? []
            hosts.removeAll { $0 == key }
            if authenticated { hosts.append(key) }
            defaults.set(hosts, forKey: "connectedHosts")
            if authenticated {
                connection.window?.makeKeyAndOrderFront(nil)
                selector?.close()
            } else {
                showHosts()
                if restoring { connection.window?.close() }
            }
        }
        window.contentView = NSHostingView(rootView: HostScreen(connection: connection).preferredColorScheme(.dark))
        connections[key] = connection
        if !restoring { window.makeKeyAndOrderFront(nil) }
        if let route { connection.navigate(to: route) }
        connection.load()
    }

    func reloadHost() {
        connections.values.first { $0.window === NSApp.keyWindow }?.load()
    }

    func windowWillClose(_ notification: Notification) {
        guard let window = notification.object as? NSWindow else { return }
        if window === selector { selector = nil }
        if let key = connections.first(where: { $0.value.window === window })?.key {
            if !isTerminating {
                let hosts = defaults.stringArray(forKey: "connectedHosts") ?? []
                defaults.set(hosts.filter { $0 != key }, forKey: "connectedHosts")
            }
            connections.removeValue(forKey: key)?.close()
        }
    }

    private func makeWindow(title: String, size: NSSize) -> NSWindow {
        let window = HostWindow(contentRect: NSRect(origin: .zero, size: size),
                                styleMask: [.titled, .closable, .miniaturizable, .resizable],
                                backing: .buffered, defer: false)
        window.title = title
        window.tabbingMode = .disallowed
        window.isReleasedWhenClosed = false
        window.styleMask.insert(.fullSizeContentView)
        window.titleVisibility = .hidden
        window.titlebarAppearsTransparent = true
        window.backgroundColor = NSColor(srgbRed: 18.0 / 255, green: 23.0 / 255, blue: 28.0 / 255, alpha: 1)
        window.collectionBehavior.insert(.fullScreenPrimary)
        window.appearance = NSAppearance(named: .darkAqua)
        window.delegate = self
        window.center()
        return window
    }
}

final class HostWindow: NSWindow {
    var headerDragStart: CGFloat?

    override func sendEvent(_ event: NSEvent) {
        if event.type == .leftMouseDown, event.clickCount == 1,
           let start = headerDragStart, let contentView,
           event.locationInWindow.x >= start,
           event.locationInWindow.y >= contentView.bounds.height - 30 {
            performDrag(with: event)
        } else {
            super.sendEvent(event)
        }
    }
}
