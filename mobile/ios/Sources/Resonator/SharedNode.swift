// A node directory shared by every app on the device (docs/shared-node.md).
//
// One app HOSTS the node in its own Documents folder, which the Files app
// shows under the app's name (Info.plist: UIFileSharingEnabled = YES,
// LSSupportsOpeningDocumentsInPlace = YES). Every other app JOINS by
// letting the user pick that folder once in the Files picker; the pick is
// kept as a bookmark, so later launches open the node directly. Whoever is
// in the foreground serves the node (the lease); an app about to be
// suspended must let go of every handle on the folder first, or iOS
// terminates it (0xdead10cc). This helper wires all of that to the app
// lifecycle. Every Node call blocks: keep them off the main thread.
//
// This file is part of the Resonator package target (build-ios.sh leaves
// it alone: it only refreshes resonator_ffi.swift beside it).

import Foundation
#if canImport(UIKit)
import UIKit
#endif

public enum SharedNodeError: Error {
    /// The picked folder holds no node (no rsntr.key / rsntr.db).
    case notANode(URL)
    /// The stored bookmark no longer resolves; pick the folder again.
    case bookmarkStale
    /// The user cancelled the picker.
    case cancelled
}

/// The node every app on the device shares, in one of two roles.
public final class SharedNode {
    /// The node directory name inside the host's Documents.
    public static let directoryName = "rsntr"
    static let bookmarkKey = "network.resonator.sharedNode.bookmark"

    public let node: Node
    public let directory: URL
    /// The directory came from a Files pick (joiner) rather than this
    /// app's own Documents (host).
    public let joined: Bool
    /// Runs after every resume: entrainments end on suspend, so re-entrain
    /// here. Called on the helper's queue.
    public var onResume: ((Node) -> Void)?
    public var onSuspend: (() -> Void)?

    private let queue = DispatchQueue(label: "network.resonator.shared-node")
    private var observers: [NSObjectProtocol] = []
    private var accessing = false

    // MARK: - Opening

    /// HOST role: this app's own `Documents/rsntr`, created on first use
    /// with the protection class that keeps it readable while the device
    /// is locked after first unlock, and excluded from backup (the key is
    /// this device's identity; a restored backup would clone it).
    public static func host(app: AppInfo, offline: Bool = false, policy: ServePolicy = .serveIfFree,
                            wireLifecycle: Bool = true) throws -> SharedNode {
        let dir = try hostDirectory()
        let node = try Node.open(dir: dir.path, offline: offline, app: app, policy: policy)
        return SharedNode(node: node, directory: dir, joined: false, wireLifecycle: wireLifecycle)
    }

    /// The host's directory (created), without opening the node.
    public static func hostDirectory() throws -> URL {
        let docs = FileManager.default.urls(for: .documentDirectory, in: .userDomainMask)[0]
        let dir = docs.appendingPathComponent(directoryName, isDirectory: true)
        if !FileManager.default.fileExists(atPath: dir.path) {
            try FileManager.default.createDirectory(
                at: dir, withIntermediateDirectories: true,
                attributes: [.protectionKey: FileProtectionType.completeUntilFirstUserAuthentication])
        }
        var values = URLResourceValues()
        values.isExcludedFromBackup = true
        var mutable = dir
        try? mutable.setResourceValues(values)
        relaxProtection(under: dir)
        return dir
    }

    /// JOINER role, later launches: opens the folder the user picked
    /// earlier (`join`), or throws `bookmarkStale` when it must be picked
    /// again. Returns nil when nothing was ever picked.
    public static func openJoined(app: AppInfo, offline: Bool = false, policy: ServePolicy = .serveIfFree,
                                  wireLifecycle: Bool = true) throws -> SharedNode? {
        guard let data = UserDefaults.standard.data(forKey: bookmarkKey) else { return nil }
        var stale = false
        guard let url = try? URL(resolvingBookmarkData: data, options: [], relativeTo: nil,
                                 bookmarkDataIsStale: &stale) else {
            throw SharedNodeError.bookmarkStale
        }
        if stale, let fresh = try? url.bookmarkData() {
            UserDefaults.standard.set(fresh, forKey: bookmarkKey)
        }
        return try open(pickedFolder: url, app: app, offline: offline, policy: policy,
                        wireLifecycle: wireLifecycle)
    }

    /// JOINER role, first time: opens the folder the user picked (verify it
    /// is a node, keep the bookmark) and the node in it. Use it from the
    /// `UIDocumentPickerViewController` delegate (`forOpeningContentTypes:
    /// [.folder]`, `asCopy: false`); see `pickerController`.
    public static func open(pickedFolder url: URL, app: AppInfo, offline: Bool = false,
                            policy: ServePolicy = .serveIfFree, wireLifecycle: Bool = true) throws -> SharedNode {
        let started = url.startAccessingSecurityScopedResource()
        defer { if started { url.stopAccessingSecurityScopedResource() } }
        let key = url.appendingPathComponent("rsntr.key").path
        let db = url.appendingPathComponent("rsntr.db").path
        guard FileManager.default.fileExists(atPath: key), FileManager.default.fileExists(atPath: db) else {
            throw SharedNodeError.notANode(url)
        }
        UserDefaults.standard.set(try url.bookmarkData(), forKey: bookmarkKey)
        let node = try Node.open(dir: url.path, offline: offline, app: app, policy: policy)
        let shared = SharedNode(node: node, directory: url, joined: true, wireLifecycle: wireLifecycle)
        shared.beginAccess()
        return shared
    }

    /// Forgets the joined folder (the next launch hosts or asks again).
    public static func forgetJoined() {
        UserDefaults.standard.removeObject(forKey: bookmarkKey)
    }

    #if canImport(UIKit)
    /// A picker configured for joining: present it, and in its delegate's
    /// `documentPicker(_:didPickDocumentsAt:)` call `open(pickedFolder:)`.
    public static func pickerController() -> UIDocumentPickerViewController {
        let picker = UIDocumentPickerViewController(forOpeningContentTypes: [.folder], asCopy: false)
        picker.allowsMultipleSelection = false
        return picker
    }
    #endif

    private init(node: Node, directory: URL, joined: Bool, wireLifecycle: Bool) {
        self.node = node
        self.directory = directory
        self.joined = joined
        if wireLifecycle { wire() }
    }

    deinit {
        observers.forEach { NotificationCenter.default.removeObserver($0) }
    }

    // MARK: - Lifecycle

    /// Lets go of every handle on the folder: serving, entrainments, the
    /// database. Called automatically when the app enters the background;
    /// safe to call by hand. Entrainments end with `onEnd("closed")`.
    public func suspend() {
        queue.sync {
            node.suspend()
            endAccess()
            onSuspend?()
        }
    }

    /// Comes back: re-acquires folder access, takes the serving lease when
    /// the policy allows and nobody holds it, and runs `onResume` so the
    /// app can re-entrain. Returns who serves now.
    @discardableResult
    public func resume() -> ServingInfo? {
        queue.sync {
            beginAccess()
            let info = try? node.resume()
            onResume?(node)
            return info ?? nil
        }
    }

    /// Ends this app's use of the node for good (`willTerminate`).
    public func close() {
        queue.sync {
            node.suspend()
            endAccess()
        }
    }

    private func beginAccess() {
        if joined, !accessing {
            accessing = directory.startAccessingSecurityScopedResource()
        }
    }

    private func endAccess() {
        if accessing {
            directory.stopAccessingSecurityScopedResource()
            accessing = false
        }
    }

    private func wire() {
        #if canImport(UIKit)
        let nc = NotificationCenter.default
        observers.append(nc.addObserver(forName: UIApplication.didEnterBackgroundNotification,
                                        object: nil, queue: .main) { [weak self] _ in
            guard let self else { return }
            // Finish before the system suspends us: a lock held on a file
            // in a shared folder at suspension is fatal.
            let task = UIApplication.shared.beginBackgroundTask(withName: "rsntr.suspend") {}
            self.queue.async {
                self.node.suspend()
                self.endAccess()
                self.onSuspend?()
                UIApplication.shared.endBackgroundTask(task)
            }
        })
        for name in [UIApplication.willEnterForegroundNotification,
                     UIApplication.protectedDataDidBecomeAvailableNotification] {
            observers.append(nc.addObserver(forName: name, object: nil, queue: .main) { [weak self] _ in
                self?.queue.async { _ = self?.resume() }
            })
        }
        observers.append(nc.addObserver(forName: UIApplication.willTerminateNotification,
                                        object: nil, queue: .main) { [weak self] _ in
            self?.close()
        })
        #endif
    }

    /// Files created under NSFileProtectionComplete (an app with the Data
    /// Protection capability at its default) become unreadable when the
    /// device locks, which breaks a node another app serves in the
    /// background. Re-apply the shared class once.
    private static func relaxProtection(under dir: URL) {
        let fm = FileManager.default
        guard let items = fm.enumerator(at: dir, includingPropertiesForKeys: nil) else { return }
        for case let item as URL in items {
            if let attrs = try? fm.attributesOfItem(atPath: item.path),
               let cls = attrs[.protectionKey] as? FileProtectionType, cls == .complete {
                try? fm.setAttributes([.protectionKey: FileProtectionType.completeUntilFirstUserAuthentication],
                                      ofItemAtPath: item.path)
            }
        }
    }
}
