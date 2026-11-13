// Starts and stops the node and follows the app's life: background,
// foreground, push wakes, pairing links and the push token.

import Foundation
import UIKit

@MainActor
final class NodeHost: ObservableObject {
    static let shared = NodeHost()

    enum State: Equatable {
        case starting
        case ready(URL)
        case busy(String)
        case failed(String)
    }

    @Published private(set) var state: State = .starting
    /// Bumped to make the WebView load the start page again.
    @Published private(set) var generation = 0

    private var node: ResonatorNode?
    private var starting = false
    /// Calls run one after another, off the main thread.
    private let queue = DispatchQueue(label: "am.resonator.client.node", qos: .userInitiated)

    private static let pushTokenKey = "pushToken"

    /// Settings laid over the device defaults (which allow files up to
    /// 4 GiB): the iroh relay the node keeps a home on, and the push relay
    /// that wakes this device when another needs it (docs/push-relay.md).
    /// Both come from the build settings RESONATOR_IROH_RELAY and
    /// RESONATOR_PUSH_RELAY (Info.plist); an empty value leaves the
    /// default (n0's public relays; push off).
    private static var extraToml: String {
        var toml = ""
        if let relay = infoString("ResonatorIrohRelay") {
            toml += "[federation]\nrelay = \"\(relay)\"\n"
        }
        if let relay = infoString("ResonatorPushRelay") {
            toml += "[push]\nrelay = \"\(relay)\"\n"
        }
        return toml
    }

    private static func infoString(_ key: String) -> String? {
        guard let value = Bundle.main.object(forInfoDictionaryKey: key) as? String else { return nil }
        let trimmed = value.trimmingCharacters(in: .whitespaces)
        return trimmed.isEmpty || trimmed.contains("\"") ? nil : trimmed
    }

    /// The node's directory: Application Support, not backed up (the keys
    /// of this device must not be restored onto another one).
    private static func dataDirectory() throws -> URL {
        var dir = try FileManager.default.url(
            for: .applicationSupportDirectory, in: .userDomainMask, appropriateFor: nil, create: true
        ).appendingPathComponent("resonator", isDirectory: true)
        try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        var values = URLResourceValues()
        values.isExcludedFromBackup = true
        try dir.setResourceValues(values)
        return dir
    }

    func start() {
        guard node == nil, !starting else { return }
        starting = true
        state = .starting
        let name = UIDevice.current.name
        queue.async {
            let result: Result<ResonatorNode, Error> = Result {
                let dir = try NodeHost.dataDirectory()
                return try ResonatorNode.start(
                    config: NodeConfig(
                        dataDir: dir.path, account: nil, deviceName: name, allowRemote: false,
                        extraToml: NodeHost.extraToml))
            }
            Task { @MainActor in
                self.starting = false
                switch result {
                case .success(let node):
                    self.node = node
                    self.sendStoredPushToken()
                    self.showStartPage()
                case .failure(let error):
                    self.state = .failed("The node did not start: \(error.localizedDescription)")
                }
            }
        }
    }

    func retry() {
        if node == nil {
            start()
        } else {
            showStartPage()
        }
    }

    /// The node's sign-in URL (with this start's token) for the WebView.
    private func showStartPage() {
        guard let node else { return }
        if let text = node.signedInUrl(), let url = URL(string: text) {
            state = .ready(url)
            generation += 1
        } else {
            state = .failed("The node gave no address to open.")
        }
    }

    /// Whether `url` is the node's own (stays in the WebView).
    func isNodeURL(_ url: URL) -> Bool {
        guard case .ready(let start) = state else { return false }
        return url.host == start.host && url.port == start.port
    }

    // MARK: Life cycle

    func background() {
        guard let node else { return }
        let app = UIApplication.shared
        var task: UIBackgroundTaskIdentifier = .invalid
        task = app.beginBackgroundTask(withName: "resonator-handover") {
            app.endBackgroundTask(task)
            task = .invalid
        }
        queue.async {
            _ = try? node.enterBackground(budgetMs: 20_000)
            DispatchQueue.main.async {
                if task != .invalid { app.endBackgroundTask(task) }
            }
        }
    }

    func foreground() {
        guard let node else {
            start()
            return
        }
        if !node.isRunning() {
            self.node = nil
            start()
            return
        }
        queue.async { _ = try? node.enterForeground(budgetMs: 10_000) }
        // iOS may close the node's listening socket while the app is
        // suspended; the page then cannot reach it ("Connecting…" for
        // ever). Ask the node; if it does not answer, start it again (a
        // new port, and the page loads afresh).
        guard case .ready(let url) = state else { return }
        Task { @MainActor in
            if await NodeHost.answers(url) {
                NSLog("resonator: foreground: node answers /healthz")
                return
            }
            NSLog("resonator: foreground: node does not answer /healthz; restarting it")
            guard self.node === node else { return }
            self.node = nil
            self.queue.async { node.stop() }
            self.start()
        }
    }

    /// Whether the node at `start` answers `/healthz` (within 3 s, twice).
    private static func answers(_ start: URL) async -> Bool {
        var components = URLComponents(url: start, resolvingAgainstBaseURL: false)
        components?.path = "/healthz"
        components?.query = nil
        guard let url = components?.url else { return true }
        let configuration = URLSessionConfiguration.ephemeral
        configuration.timeoutIntervalForRequest = 3
        let session = URLSession(configuration: configuration)
        defer { session.finishTasksAndInvalidate() }
        for _ in 0..<2 {
            if let (_, response) = try? await session.data(from: url),
                let http = response as? HTTPURLResponse, http.statusCode < 500
            {
                return true
            }
        }
        return false
    }

    /// A silent push: about 30 seconds to catch up and hand over again.
    func wake(_ done: @escaping (Bool) -> Void) {
        guard let node else {
            done(false)
            return
        }
        queue.async {
            let synced = try? node.enterForeground(budgetMs: 15_000)
            if UIApplication.shared.applicationState != .active {
                _ = try? node.enterBackground(budgetMs: 8_000)
            }
            let changed = (synced?.waveletsUpdated ?? 0) > 0 || (synced?.pendingSent ?? 0) > 0
            DispatchQueue.main.async { done(changed) }
        }
    }

    // MARK: Push token

    func pushToken(_ hex: String) {
        UserDefaults.standard.set(hex, forKey: Self.pushTokenKey)
        sendStoredPushToken()
    }

    private func sendStoredPushToken() {
        guard let node, let hex = UserDefaults.standard.string(forKey: Self.pushTokenKey) else { return }
        #if DEBUG
            let platform = PushPlatform.apnsSandbox
        #else
            let platform = PushPlatform.apns
        #endif
        queue.async { try? node.setPushToken(platform: platform, token: hex) }
    }

    // MARK: Links

    /// `resonator:pair/…` pairs this device; other links go to the client.
    func open(_ url: URL) {
        let text = url.absoluteString
        if text.hasPrefix("resonator:pair/") {
            pair(text)
        }
    }

    private func pair(_ link: String) {
        guard let node else { return }
        state = .busy("Pairing this device…")
        let name = UIDevice.current.name
        queue.async {
            let result = Result { try node.pairJoin(codeOrLink: link, deviceName: name, waitMs: 180_000) }
            Task { @MainActor in
                switch result {
                case .success:
                    // The node restarted as a device of that person, at
                    // the same address with the same token.
                    self.showStartPage()
                case .failure(let error):
                    self.state = .failed("Pairing failed: \(error.localizedDescription)")
                }
            }
        }
    }
}
