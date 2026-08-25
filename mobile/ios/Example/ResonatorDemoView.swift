// Minimal example view: drop into an iOS app that depends on the
// Resonator Swift package (mobile/ios). Creates a node in the app's
// documents directory, runs a local SQL statement, entrains a
// Sympathetic point, and shows vibrations arriving live.
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
                let dir = FileManager.default
                    .urls(for: .documentDirectory, in: .userDomainMask)[0]
                    .appendingPathComponent("resonator-node").path
                let n = try Node(dir: dir, offline: false)
                let id = try n.endpointId()
                _ = try n.localExecute(
                    signal: "CREATE TABLE IF NOT EXISTS notes (id INTEGER PRIMARY KEY, body TEXT)",
                    params: [])
                _ = try n.localExecute(
                    signal: """
                        INSERT OR IGNORE INTO _projection (point_iri, kind, label, resource) \
                        VALUES ('urn:demo:notes-changed', 'sympathetic', 'notes changed', 'notes')
                        """,
                    params: [])
                let s = try n.entrain(point: "urn:demo:notes-changed", listener: model)
                DispatchQueue.main.async {
                    node = n
                    session = s
                    endpointId = id
                    status = "entrained urn:demo:notes-changed"
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
                signal: "INSERT INTO notes (body) VALUES (?1)",
                params: [.text(v: "hello at \(Date())")])
        }
    }
}
