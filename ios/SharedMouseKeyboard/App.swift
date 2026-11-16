import SwiftUI

@main
struct SharedMouseKeyboardApp: App {
    @StateObject private var model = Model()

    var body: some Scene {
        WindowGroup {
            Root()
                .environmentObject(model)
                .onAppear { model.start() }
        }
    }
}

struct Root: View {
    @EnvironmentObject var model: Model

    var body: some View {
        if let error = model.startError {
            Text("The engine could not start: \(error)").padding()
        } else if let state = model.state {
            content(state)
                .sheet(item: Binding(get: { state.peers.first(where: { $0.pairing != nil }) }, set: { _ in })) { peer in
                    PairingSheet(peer: peer).environmentObject(model)
                }
        } else {
            ProgressView()
        }
    }

    @ViewBuilder
    private func content(_ state: UiState) -> some View {
        if state.controlling != nil {
            TouchpadView(state: state)
        } else {
            TabView {
                MachinesView(state: state)
                    .tabItem { Label("Machines", systemImage: "desktopcomputer") }
                LayoutView(state: state)
                    .tabItem { Label("Layout", systemImage: "square.grid.2x2") }
            }
        }
    }
}
