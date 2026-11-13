// Resonator for macOS: the web client of a resonator node on this Mac in
// its own window.
//
// The window shows the node at `ServerURL` (Info.plist, or the user
// default of the same name), or, for a node in device mode, at the URL
// the node writes to the file `ServerURLFile` at every start (it carries
// the token that signs the device's one account in; it is never logged).
// If nothing answers there and `ServerCommand` is set, the app starts the
// node itself (in `ServerDirectory`, with the array `ServerArguments`) and
// leaves it running when it quits, so that other devices and servers keep
// reaching it.

import AppKit
import UniformTypeIdentifiers
import WebKit

func setting(_ key: String) -> String? {
    if let value = UserDefaults.standard.string(forKey: key), !value.isEmpty {
        return value
    }
    if let value = Bundle.main.object(forInfoDictionaryKey: key) as? String, !value.isEmpty {
        return value
    }
    return nil
}

final class AppDelegate: NSObject, NSApplicationDelegate, WKNavigationDelegate, WKUIDelegate,
    WKDownloadDelegate
{
    var serverURL = URL(string: setting("ServerURL") ?? "http://127.0.0.1:9899/")!
    var window: NSWindow!
    var webView: WKWebView!
    var status: NSTextField!
    var startedServer = false
    var rereadAfterStart = false
    var attempts = 0

    func applicationDidFinishLaunching(_ notification: Notification) {
        buildMenu()
        let configuration = WKWebViewConfiguration()
        configuration.websiteDataStore = .default()
        configuration.preferences.setValue(true, forKey: "developerExtrasEnabled")
        webView = WKWebView(frame: .zero, configuration: configuration)
        webView.navigationDelegate = self
        webView.uiDelegate = self
        webView.allowsBackForwardNavigationGestures = true
        webView.autoresizingMask = [.width, .height]

        window = NSWindow(
            contentRect: NSRect(x: 0, y: 0, width: 1280, height: 860),
            styleMask: [.titled, .closable, .miniaturizable, .resizable],
            backing: .buffered, defer: false)
        window.title = "Resonator"
        window.minSize = NSSize(width: 480, height: 400)
        window.isReleasedWhenClosed = false
        window.center()
        window.setFrameAutosaveName("ResonatorMain")
        webView.frame = window.contentView!.bounds
        window.contentView!.addSubview(webView)

        status = NSTextField(labelWithString: "Connecting to the resonator node…")
        status.alignment = .center
        status.textColor = .secondaryLabelColor
        status.font = .systemFont(ofSize: 15)
        status.maximumNumberOfLines = 0
        status.translatesAutoresizingMaskIntoConstraints = false
        window.contentView!.addSubview(status)
        NSLayoutConstraint.activate([
            status.centerXAnchor.constraint(equalTo: window.contentView!.centerXAnchor),
            status.centerYAnchor.constraint(equalTo: window.contentView!.centerYAnchor),
            status.widthAnchor.constraint(lessThanOrEqualTo: window.contentView!.widthAnchor, constant: -80),
        ])
        webView.isHidden = true

        window.makeKeyAndOrderFront(nil)
        NSApp.activate(ignoringOtherApps: true)
        connect()
    }

    // MARK: Reaching (or starting) the node

    /// Device mode: the sign-in URL the node wrote at its last start.
    func readURLFile() {
        guard let path = setting("ServerURLFile"),
            let text = try? String(contentsOfFile: path, encoding: .utf8),
            let url = URL(string: text.trimmingCharacters(in: .whitespacesAndNewlines))
        else { return }
        serverURL = url
    }

    func connect() {
        attempts += 1
        readURLFile()
        var request = URLRequest(url: serverURL)
        request.timeoutInterval = 2
        request.cachePolicy = .reloadIgnoringLocalCacheData
        URLSession.shared.dataTask(with: request) { _, response, _ in
            DispatchQueue.main.async {
                if response is HTTPURLResponse {
                    // A node that has just started may have answered
                    // before it wrote this start's URL.
                    if self.startedServer && !self.rereadAfterStart {
                        self.rereadAfterStart = true
                        DispatchQueue.main.asyncAfter(deadline: .now() + 0.7) { self.connect() }
                        return
                    }
                    self.status.isHidden = true
                    self.webView.isHidden = false
                    self.webView.load(URLRequest(url: self.serverURL))
                    return
                }
                if !self.startedServer {
                    self.startedServer = true
                    self.startServer()
                }
                if self.attempts > 30 {
                    self.status.stringValue =
                        "No resonator node answers at \(self.serverURL.host ?? ""):\(self.serverURL.port ?? 80).\n"
                        + "Start it and choose View › Reload (⌘R)."
                    return
                }
                DispatchQueue.main.asyncAfter(deadline: .now() + 0.5) { self.connect() }
            }
        }.resume()
    }

    /// Starts the node detached from the app, appending its output to
    /// `ServerLog`.
    func startServer() {
        guard let command = setting("ServerCommand"), let directory = setting("ServerDirectory")
        else { return }
        let directoryURL = URL(fileURLWithPath: directory)
        let executable = URL(fileURLWithPath: command, relativeTo: directoryURL)
        guard FileManager.default.isExecutableFile(atPath: executable.path) else { return }
        status.stringValue = "Starting the resonator node…"
        let process = Process()
        process.executableURL = executable
        process.currentDirectoryURL = directoryURL
        process.arguments =
            Bundle.main.object(forInfoDictionaryKey: "ServerArguments") as? [String] ?? ["serve"]
        if let log = setting("ServerLog") {
            let logURL = URL(fileURLWithPath: log, relativeTo: directoryURL)
            if !FileManager.default.fileExists(atPath: logURL.path) {
                FileManager.default.createFile(atPath: logURL.path, contents: nil)
            }
            if let handle = try? FileHandle(forWritingTo: logURL) {
                handle.seekToEndOfFile()
                process.standardOutput = handle
                process.standardError = handle
            }
        }
        process.standardInput = FileHandle.nullDevice
        try? process.run()
    }

    @objc func reload(_ sender: Any?) {
        if webView.isHidden || webView.url == nil {
            attempts = 0
            status.stringValue = "Connecting to the resonator node…"
            connect()
        } else {
            webView.reload()
        }
    }

    @objc func goBack(_ sender: Any?) { webView.goBack() }
    @objc func goForward(_ sender: Any?) { webView.goForward() }

    @objc func openInBrowser(_ sender: Any?) {
        // The browser has its own cookies: with a device's token it is signed in as well.
        NSWorkspace.shared.open(setting("ServerURLFile") != nil ? serverURL : webView.url ?? serverURL)
    }

    @objc func home(_ sender: Any?) {
        var parts = URLComponents(url: serverURL, resolvingAgainstBaseURL: false)
        parts?.query = nil
        webView.load(URLRequest(url: parts?.url ?? serverURL))
    }

    @objc func zoomIn(_ sender: Any?) { webView.pageZoom = min(webView.pageZoom + 0.1, 3) }
    @objc func zoomOut(_ sender: Any?) { webView.pageZoom = max(webView.pageZoom - 0.1, 0.5) }
    @objc func zoomReset(_ sender: Any?) { webView.pageZoom = 1 }

    func applicationShouldTerminateAfterLastWindowClosed(_ sender: NSApplication) -> Bool { true }

    // MARK: Navigation: the node stays in the window, the rest goes to the browser

    func isNode(_ url: URL) -> Bool {
        url.host == serverURL.host && url.port == serverURL.port
    }

    func webView(
        _ webView: WKWebView, decidePolicyFor action: WKNavigationAction,
        decisionHandler: @escaping (WKNavigationActionPolicy) -> Void
    ) {
        guard let url = action.request.url else { return decisionHandler(.cancel) }
        // Attachments (other than images, which the client shows itself) are
        // saved, not opened in place of the client.
        if action.shouldPerformDownload || (isNode(url) && url.path.hasPrefix("/attachment/")) {
            return decisionHandler(.download)
        }
        if isNode(url) || url.scheme == "about" || url.scheme == "blob" || url.scheme == "data" {
            return decisionHandler(.allow)
        }
        if action.targetFrame?.isMainFrame ?? true { NSWorkspace.shared.open(url) }
        decisionHandler(.cancel)
    }

    func webView(
        _ webView: WKWebView, decidePolicyFor response: WKNavigationResponse,
        decisionHandler: @escaping (WKNavigationResponsePolicy) -> Void
    ) {
        let http = response.response as? HTTPURLResponse
        let disposition = http?.value(forHTTPHeaderField: "Content-Disposition") ?? ""
        if !response.canShowMIMEType || disposition.lowercased().hasPrefix("attachment") {
            decisionHandler(.download)
        } else {
            decisionHandler(.allow)
        }
    }

    func webView(
        _ webView: WKWebView, createWebViewWith configuration: WKWebViewConfiguration,
        for action: WKNavigationAction, windowFeatures: WKWindowFeatures
    ) -> WKWebView? {
        // target="_blank": the node's pages in this window, others in the browser.
        if let url = action.request.url {
            if isNode(url) && url.path.hasPrefix("/attachment/") {
                webView.startDownload(using: action.request) { $0.delegate = self }
            } else if isNode(url) {
                webView.load(action.request)
            } else {
                NSWorkspace.shared.open(url)
            }
        }
        return nil
    }

    func webView(
        _ webView: WKWebView, didFailProvisionalNavigation navigation: WKNavigation!,
        withError error: Error
    ) {
        let code = (error as NSError).code
        if code == NSURLErrorCancelled || code == 102 { return }  // 102: became a download
        webView.isHidden = true
        status.isHidden = false
        status.stringValue = "\(error.localizedDescription)\nChoose View › Reload (⌘R) to try again."
        attempts = 31
    }

    func webView(_ webView: WKWebView, didFinish navigation: WKNavigation!) {
        if let title = webView.title, !title.isEmpty { window.title = title }
    }

    // MARK: Downloads

    func webView(_ webView: WKWebView, navigationAction: WKNavigationAction, didBecome download: WKDownload) {
        download.delegate = self
    }

    func webView(_ webView: WKWebView, navigationResponse: WKNavigationResponse, didBecome download: WKDownload) {
        download.delegate = self
    }

    func download(
        _ download: WKDownload, decideDestinationUsing response: URLResponse,
        suggestedFilename: String, completionHandler: @escaping (URL?) -> Void
    ) {
        let panel = NSSavePanel()
        panel.nameFieldStringValue = suggestedFilename
        panel.directoryURL = FileManager.default.urls(for: .downloadsDirectory, in: .userDomainMask).first
        panel.beginSheetModal(for: window) { result in
            guard result == .OK, let url = panel.url else { return completionHandler(nil) }
            try? FileManager.default.removeItem(at: url)  // the panel asked about replacing
            completionHandler(url)
        }
    }

    // MARK: Page dialogs and file choosers

    func webView(
        _ webView: WKWebView, runOpenPanelWith parameters: WKOpenPanelParameters,
        initiatedByFrame frame: WKFrameInfo, completionHandler: @escaping ([URL]?) -> Void
    ) {
        let panel = NSOpenPanel()
        panel.allowsMultipleSelection = parameters.allowsMultipleSelection
        panel.canChooseDirectories = parameters.allowsDirectories
        panel.beginSheetModal(for: window) { result in
            completionHandler(result == .OK ? panel.urls : nil)
        }
    }

    func webView(
        _ webView: WKWebView, runJavaScriptAlertPanelWithMessage message: String,
        initiatedByFrame frame: WKFrameInfo, completionHandler: @escaping () -> Void
    ) {
        let alert = NSAlert()
        alert.messageText = message
        alert.beginSheetModal(for: window) { _ in completionHandler() }
    }

    func webView(
        _ webView: WKWebView, runJavaScriptConfirmPanelWithMessage message: String,
        initiatedByFrame frame: WKFrameInfo, completionHandler: @escaping (Bool) -> Void
    ) {
        let alert = NSAlert()
        alert.messageText = message
        alert.addButton(withTitle: "OK")
        alert.addButton(withTitle: "Cancel")
        alert.beginSheetModal(for: window) { completionHandler($0 == .alertFirstButtonReturn) }
    }

    func webView(
        _ webView: WKWebView, runJavaScriptTextInputPanelWithPrompt prompt: String,
        defaultText: String?, initiatedByFrame frame: WKFrameInfo,
        completionHandler: @escaping (String?) -> Void
    ) {
        let alert = NSAlert()
        alert.messageText = prompt
        let field = NSTextField(frame: NSRect(x: 0, y: 0, width: 300, height: 24))
        field.stringValue = defaultText ?? ""
        alert.accessoryView = field
        alert.addButton(withTitle: "OK")
        alert.addButton(withTitle: "Cancel")
        alert.beginSheetModal(for: window) {
            completionHandler($0 == .alertFirstButtonReturn ? field.stringValue : nil)
        }
    }

    // MARK: Menu

    func buildMenu() {
        let main = NSMenu()
        func menu(_ title: String, _ items: [NSMenuItem]) {
            let item = NSMenuItem()
            let submenu = NSMenu(title: title)
            items.forEach(submenu.addItem)
            item.submenu = submenu
            main.addItem(item)
        }
        func item(_ title: String, _ action: Selector?, _ key: String = "", _ modifiers: NSEvent.ModifierFlags = .command) -> NSMenuItem {
            let item = NSMenuItem(title: title, action: action, keyEquivalent: key)
            item.keyEquivalentModifierMask = modifiers
            return item
        }
        menu("Resonator", [
            item("About Resonator", #selector(NSApplication.orderFrontStandardAboutPanel(_:))),
            .separator(),
            item("Hide Resonator", #selector(NSApplication.hide(_:)), "h"),
            item("Hide Others", #selector(NSApplication.hideOtherApplications(_:)), "h", [.command, .option]),
            .separator(),
            item("Quit Resonator", #selector(NSApplication.terminate(_:)), "q"),
        ])
        menu("Edit", [
            item("Undo", Selector(("undo:")), "z"),
            item("Redo", Selector(("redo:")), "z", [.command, .shift]),
            .separator(),
            item("Cut", #selector(NSText.cut(_:)), "x"),
            item("Copy", #selector(NSText.copy(_:)), "c"),
            item("Paste", #selector(NSText.paste(_:)), "v"),
            item("Paste and Match Style", #selector(NSTextView.pasteAsPlainText(_:)), "v", [.command, .option, .shift]),
            item("Select All", #selector(NSText.selectAll(_:)), "a"),
        ])
        menu("View", [
            item("Back", #selector(goBack(_:)), "["),
            item("Forward", #selector(goForward(_:)), "]"),
            .separator(),
            item("Reload", #selector(reload(_:)), "r"),
            item("Inbox", #selector(home(_:)), "H", [.command, .shift]),
            .separator(),
            item("Actual Size", #selector(zoomReset(_:)), "0"),
            item("Zoom In", #selector(zoomIn(_:)), "+"),
            item("Zoom Out", #selector(zoomOut(_:)), "-"),
            .separator(),
            item("Open in Browser", #selector(openInBrowser(_:)), "b", [.command, .shift]),
            item("Enter Full Screen", #selector(NSWindow.toggleFullScreen(_:)), "f", [.command, .control]),
        ])
        menu("Window", [
            item("Minimize", #selector(NSWindow.performMiniaturize(_:)), "m"),
            item("Close", #selector(NSWindow.performClose(_:)), "w"),
        ])
        NSApp.mainMenu = main
    }
}

let app = NSApplication.shared
let delegate = AppDelegate()
app.delegate = delegate
app.setActivationPolicy(.regular)
app.run()
