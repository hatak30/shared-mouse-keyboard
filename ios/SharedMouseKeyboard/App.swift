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
            if state.controlling != nil {
                TouchpadView(state: state)
            } else {
                MachinesView(state: state)
            }
        } else {
            ProgressView()
        }
    }
}
