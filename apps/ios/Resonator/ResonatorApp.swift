// Resonator for iOS: a resonator node of this phone, in device mode, with
// the web client it serves on 127.0.0.1 shown in a WebView.
//
// The node (crates/resonator-ffi) is a device of a person: on the first
// start it makes a person key of its own; pairing (Settings in the client,
// or a resonator:pair/… link opened here) makes it a device of another.
// The app hands the node over when it goes to the background and lets it
// sync when it comes back or a silent push wakes it.

import SwiftUI
import UIKit

@main
struct ResonatorApp: App {
    @UIApplicationDelegateAdaptor(AppDelegate.self) var appDelegate
    @Environment(\.scenePhase) private var scenePhase

    var body: some Scene {
        WindowGroup {
            ContentView(host: NodeHost.shared)
                .onOpenURL { url in NodeHost.shared.open(url) }
        }
        .onChange(of: scenePhase) { phase in
            switch phase {
            case .active: NodeHost.shared.foreground()
            case .background: NodeHost.shared.background()
            default: break
            }
        }
    }
}

final class AppDelegate: NSObject, UIApplicationDelegate {
    func application(
        _ application: UIApplication,
        didFinishLaunchingWithOptions launchOptions: [UIApplication.LaunchOptionsKey: Any]? = nil
    ) -> Bool {
        NodeHost.shared.start()
        // Silent pushes need no permission prompt.
        application.registerForRemoteNotifications()
        return true
    }

    func application(_ application: UIApplication, didRegisterForRemoteNotificationsWithDeviceToken deviceToken: Data) {
        NodeHost.shared.pushToken(deviceToken.map { String(format: "%02x", $0) }.joined())
    }

    func application(_ application: UIApplication, didFailToRegisterForRemoteNotificationsWithError error: Error) {
        // The simulator, or no push entitlement: the node works without.
    }

    /// A wake from the push relay: sync, send what is queued, hand over.
    func application(
        _ application: UIApplication,
        didReceiveRemoteNotification userInfo: [AnyHashable: Any],
        fetchCompletionHandler completionHandler: @escaping (UIBackgroundFetchResult) -> Void
    ) {
        NodeHost.shared.wake { changed in completionHandler(changed ? .newData : .noData) }
    }
}
