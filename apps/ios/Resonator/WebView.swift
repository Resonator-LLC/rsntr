// The client in a WKWebView: the node's pages stay here, other links open
// in Safari, attachments and downloads open in Quick Look (which has the
// share button).

import QuickLook
import SwiftUI
import UniformTypeIdentifiers
import WebKit

struct WebView: UIViewRepresentable {
    let start: URL
    let generation: Int
    let host: NodeHost

    func makeCoordinator() -> Coordinator { Coordinator(host: host) }

    func makeUIView(context: Context) -> WKWebView {
        let configuration = WKWebViewConfiguration()
        configuration.websiteDataStore = .default()
        configuration.allowsInlineMediaPlayback = true
        let view = WKWebView(frame: .zero, configuration: configuration)
        view.navigationDelegate = context.coordinator
        view.uiDelegate = context.coordinator
        view.allowsBackForwardNavigationGestures = true
        view.isOpaque = false
        view.backgroundColor = .systemBackground
        if #available(iOS 16.4, *) {
            #if DEBUG
                view.isInspectable = true
            #endif
        }
        context.coordinator.webView = view
        return view
    }

    func updateUIView(_ view: WKWebView, context: Context) {
        if context.coordinator.loadedGeneration != generation {
            context.coordinator.loadedGeneration = generation
            view.load(URLRequest(url: start))
        }
    }

    final class Coordinator: NSObject, WKNavigationDelegate, WKUIDelegate, WKDownloadDelegate,
        QLPreviewControllerDataSource
    {
        let host: NodeHost
        weak var webView: WKWebView?
        var loadedGeneration = -1
        private var downloads: [WKDownload: URL] = [:]
        private var previewing: URL?

        /// An attachment of the node (`/attachment/<id>`): shown in Quick Look.
        private func isAttachment(_ url: URL) -> Bool {
            host.isNodeURL(url) && url.path.hasPrefix("/attachment/")
        }

        init(host: NodeHost) { self.host = host }

        @MainActor
        func webView(
            _ webView: WKWebView, decidePolicyFor action: WKNavigationAction,
            decisionHandler: @escaping @MainActor (WKNavigationActionPolicy) -> Void
        ) {
            guard let url = action.request.url else { return decisionHandler(.cancel) }
            if action.shouldPerformDownload || isAttachment(url) { return decisionHandler(.download) }
            if host.isNodeURL(url) || ["about", "blob", "data"].contains(url.scheme ?? "") {
                return decisionHandler(.allow)
            }
            if url.scheme == "resonator" {
                host.open(url)
            } else if action.targetFrame?.isMainFrame ?? true {
                UIApplication.shared.open(url)
            }
            decisionHandler(.cancel)
        }

        @MainActor
        func webView(
            _ webView: WKWebView, decidePolicyFor response: WKNavigationResponse,
            decisionHandler: @escaping @MainActor (WKNavigationResponsePolicy) -> Void
        ) {
            let http = response.response as? HTTPURLResponse
            if let http, http.statusCode >= 400, !response.isForMainFrame || (http.url.map(isAttachment) ?? false) {
                // Never save an error as a file.
                decisionHandler(.cancel)
                Task { @MainActor in self.notAvailable(status: http.statusCode) }
                return
            }
            let disposition = http?.value(forHTTPHeaderField: "Content-Disposition")?.lowercased() ?? ""
            decisionHandler(!response.canShowMIMEType || disposition.hasPrefix("attachment") ? .download : .allow)
        }

        func webView(
            _ webView: WKWebView, createWebViewWith configuration: WKWebViewConfiguration,
            for action: WKNavigationAction, windowFeatures: WKWindowFeatures
        ) -> WKWebView? {
            // target="_blank": the node's pages here, the rest in Safari.
            if let url = action.request.url {
                Task { @MainActor in
                    if self.isAttachment(url) {
                        let download = await webView.startDownload(using: action.request)
                        download.delegate = self
                    } else if self.host.isNodeURL(url) {
                        webView.load(action.request)
                    } else {
                        await UIApplication.shared.open(url)
                    }
                }
            }
            return nil
        }

        /// The node went away under the page (it was stopped while the app
        /// slept): start it again.
        func webViewWebContentProcessDidTerminate(_ webView: WKWebView) {
            webView.reload()
        }

        func webView(_ webView: WKWebView, didFailProvisionalNavigation navigation: WKNavigation!, withError error: Error) {
            let code = (error as NSError).code
            if code == NSURLErrorCancelled || code == 102 { return }  // 102: became a download
            if code == NSURLErrorCannotConnectToHost || code == NSURLErrorNetworkConnectionLost {
                Task { @MainActor in self.host.foreground() }
            }
        }

        // MARK: Downloads → share sheet

        func webView(_ webView: WKWebView, navigationAction: WKNavigationAction, didBecome download: WKDownload) {
            download.delegate = self
        }

        func webView(_ webView: WKWebView, navigationResponse: WKNavigationResponse, didBecome download: WKDownload) {
            download.delegate = self
        }

        func download(
            _ download: WKDownload, decideDestinationUsing response: URLResponse, suggestedFilename: String,
            completionHandler: @escaping (URL?) -> Void
        ) {
            if let http = response as? HTTPURLResponse, http.statusCode >= 400 {
                completionHandler(nil)
                download.cancel()
                Task { @MainActor in self.notAvailable(status: http.statusCode) }
                return
            }
            let dir = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
            try? FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
            let url = dir.appendingPathComponent(Self.fileName(response: response, suggested: suggestedFilename))
            downloads[download] = url
            completionHandler(url)
        }

        /// The file's name from the server (Content-Disposition), with an
        /// extension for its type, so that Quick Look and other apps know
        /// what it is.
        static func fileName(response: URLResponse, suggested: String) -> String {
            var name = response.suggestedFilename ?? suggested
            name = name.replacingOccurrences(of: "/", with: "_")
            if name.isEmpty { name = "attachment" }
            if (name as NSString).pathExtension.isEmpty,
                let mime = response.mimeType, let ext = UTType(mimeType: mime)?.preferredFilenameExtension
            {
                name += ".\(ext)"
            }
            return name
        }

        func downloadDidFinish(_ download: WKDownload) {
            guard let url = downloads.removeValue(forKey: download) else { return }
            Task { @MainActor in self.preview(url) }
        }

        func download(_ download: WKDownload, didFailWithError error: Error, resumeData: Data?) {
            downloads.removeValue(forKey: download)
        }

        @MainActor
        private func preview(_ url: URL) {
            guard let root = webView?.window?.rootViewController else { return }
            previewing = url
            let controller = QLPreviewController()
            controller.dataSource = self
            (root.presentedViewController ?? root).present(controller, animated: true)
        }

        func numberOfPreviewItems(in controller: QLPreviewController) -> Int { previewing == nil ? 0 : 1 }

        func previewController(_ controller: QLPreviewController, previewItemAt index: Int) -> QLPreviewItem {
            previewing! as NSURL
        }

        @MainActor
        private func notAvailable(status: Int) {
            guard let view = webView else { return }
            let alert = UIAlertController(
                title: "Not available",
                message: status == 503
                    ? "The device or server that has this file cannot be reached right now. Try again when it is online."
                    : "This file cannot be opened (error \(status)).",
                preferredStyle: .alert)
            alert.addAction(UIAlertAction(title: "OK", style: .default))
            present(alert, in: view) {}
        }

        // MARK: Page dialogs

        func webView(
            _ webView: WKWebView, runJavaScriptAlertPanelWithMessage message: String,
            initiatedByFrame frame: WKFrameInfo, completionHandler: @escaping () -> Void
        ) {
            let alert = UIAlertController(title: nil, message: message, preferredStyle: .alert)
            alert.addAction(UIAlertAction(title: "OK", style: .default) { _ in completionHandler() })
            present(alert, in: webView, otherwise: completionHandler)
        }

        func webView(
            _ webView: WKWebView, runJavaScriptConfirmPanelWithMessage message: String,
            initiatedByFrame frame: WKFrameInfo, completionHandler: @escaping (Bool) -> Void
        ) {
            let alert = UIAlertController(title: nil, message: message, preferredStyle: .alert)
            alert.addAction(UIAlertAction(title: "Cancel", style: .cancel) { _ in completionHandler(false) })
            alert.addAction(UIAlertAction(title: "OK", style: .default) { _ in completionHandler(true) })
            present(alert, in: webView) { completionHandler(false) }
        }

        private func present(_ alert: UIAlertController, in view: WKWebView, otherwise: @escaping () -> Void) {
            guard let root = view.window?.rootViewController else { return otherwise() }
            (root.presentedViewController ?? root).present(alert, animated: true)
        }
    }
}
