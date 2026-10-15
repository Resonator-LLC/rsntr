// Minimal example view: drop into an iOS app that depends on the
// Resonator Swift package (mobile/ios). HOSTS the device's shared node in
// this app's Documents/rsntr (Info.plist: UIFileSharingEnabled = YES and
// LSSupportsOpeningDocumentsInPlace = YES, so other apps can pick the
// folder in Files and join), declares a table, entrains a Sympathetic
// point, and shows vibrations arriving live, including writes made by
// other apps on the same node. A second app joins with
// `SharedNode.openJoined` / the Files picker; see docs/shared-node.md.
//
// This file is example code, not part of the package target; copy it
// into your app project.

import SwiftUI
import Resonator

/// Bridges the uniffi VibrationListener callback onto SwiftUI state.
final class VibrationModel: ObservableObject, VibrationListener {
    @Published var lines: [String] = []

    func onVibration(point: String, seq: Int64, at: String?) {
        DispatchQueue.main.async {
            self.lines.append("vibration #\(seq) of \(point) at \(at ?? "?")")
        }
    }

    func onEnd(reason: String?) {
        DispatchQueue.main.async {
            self.lines.append("entrainment ended: \(reason ?? "clean")")
        }
    }
}

struct ResonatorDemoView: View {
    @StateObject private var model = VibrationModel()
    @State private var shared: SharedNode?
    @State private var node: Node?
    @State private var session: Entrainment?
    @State private var endpointId = ""
    @State private var status = "not started"

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text("resonator").font(.headline)
            Text("endpoint: \(endpointId)").font(.caption.monospaced())
            Text(status)
            Button("write a row (vibrates)") { writeRow() }
                .disabled(node == nil)
            List(model.lines.indices, id: \.self) { i in
                Text(model.lines[i]).font(.caption.monospaced())
            }
        }
        .padding()
        .task { start() }
    }

    private func start() {
        // Every Node call blocks; keep them off the main thread in a
        // real app. Node directory = one sqlite db + one key file.
        DispatchQueue.global().async {
            do {
                // A joiner would call SharedNode.openJoined(app:) and, when
                // that returns nil, present SharedNode.pickerController().
                let app = AppInfo(id: "network.resonator.demo", name: "Resonator Demo", version: "0.1")
                let sh = try SharedNode.host(app: app, offline: false)
                let n = sh.node
                let id = try n.endpointId()
                _ = try n.localExecute(
                    signal: "CREATE TABLE IF NOT EXISTS demo_notes (id INTEGER PRIMARY KEY, body TEXT)",
                    params: [])
                try n.declareTables(tables: [TableDecl(name: "demo_notes", comment: "notes typed in the demo app")])
                _ = try n.localExecute(
                    signal: """
                        INSERT OR IGNORE INTO _projection (point_iri, kind, label, resource) \
                        VALUES ('urn:demo:notes-changed', 'sympathetic', 'notes changed', 'demo_notes')
                        """,
                    params: [])
                let s = try n.entrain(point: "urn:demo:notes-changed", listener: model)
                // Entrainments end on suspend; come back with the app.
                sh.onResume = { node in
                    if let again = try? node.entrain(point: "urn:demo:notes-changed", listener: model) {
                        DispatchQueue.main.async { session = again }
                    }
                }
                _ = sh.resume()
                DispatchQueue.main.async {
                    shared = sh
                    node = n
                    session = s
                    endpointId = id
                    status = "entrained urn:demo:notes-changed (serving: \(n.servingInfo()?.appId ?? "nobody"))"
                }
            } catch {
                DispatchQueue.main.async { status = "failed: \(error)" }
            }
        }
    }

    private func writeRow() {
        guard let n = node else { return }
        DispatchQueue.global().async {
            _ = try? n.localExecute(
                signal: "INSERT INTO demo_notes (body) VALUES (?1)",
                params: [.text(v: "hello at \(Date())")])
        }
    }
}
