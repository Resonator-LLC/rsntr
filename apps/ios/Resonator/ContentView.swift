import SwiftUI

struct ContentView: View {
    @ObservedObject var host: NodeHost

    var body: some View {
        ZStack {
            Color(uiColor: .systemBackground).ignoresSafeArea()
            switch host.state {
            case .ready(let url):
                WebView(start: url, generation: host.generation, host: host)
                    .ignoresSafeArea(.container, edges: .bottom)
            case .starting:
                Waiting(text: "Starting Resonator…")
            case .busy(let text):
                Waiting(text: text)
            case .failed(let text):
                VStack(spacing: 16) {
                    Text(text).multilineTextAlignment(.center).foregroundStyle(.secondary)
                    Button("Try again") { host.retry() }.buttonStyle(.borderedProminent)
                }
                .padding(32)
            }
        }
    }
}

private struct Waiting: View {
    let text: String
    var body: some View {
        VStack(spacing: 14) {
            ProgressView()
            Text(text).foregroundStyle(.secondary)
        }
    }
}
