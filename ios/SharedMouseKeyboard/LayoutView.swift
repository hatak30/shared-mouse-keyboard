import SwiftUI

/// The shared screen layout. Tap a machine that is paired and online to control it.
struct LayoutView: View {
    @EnvironmentObject var model: Model
    let state: UiState
    @State private var selected: Placement?

    private var xs: ClosedRange<Int> {
        let v = state.layout.placements.map(\.x)
        return (v.min() ?? 0)...(v.max() ?? 0)
    }

    private var ys: ClosedRange<Int> {
        let v = state.layout.placements.map(\.y)
        return (v.min() ?? 0)...(v.max() ?? 0)
    }

    var body: some View {
        NavigationStack {
            VStack(alignment: .leading, spacing: 12) {
                Text("This is the layout shared by your computers. Tap a machine to control it from this phone.")
                    .font(.footnote).foregroundStyle(.secondary)
                ScrollView([.horizontal, .vertical]) {
                    VStack(spacing: 8) {
                        ForEach(Array(ys), id: \.self) { y in
                            HStack(spacing: 8) {
                                ForEach(Array(xs), id: \.self) { x in
                                    cell(x: x, y: y)
                                }
                            }
                        }
                    }
                    .padding(4)
                }
                legend
                Spacer(minLength: 0)
            }
            .padding()
            .navigationTitle("Layout")
            .confirmationDialog(selected?.name ?? "", isPresented: Binding(get: { selected != nil }, set: { if !$0 { selected = nil } }), titleVisibility: .visible) {
                if let p = selected, let peer = peer(for: p), peer.trusted, peer.connected, peer.os != "Ios" {
                    Button("Control \(p.name)") { model.send("control_peer", peer.nodeId) }
                }
                Button("Cancel", role: .cancel) {}
            } message: {
                if let p = selected { Text(status(of: p)) }
            }
        }
    }

    @ViewBuilder
    private func cell(x: Int, y: Int) -> some View {
        if let p = state.layout.placements.first(where: { $0.x == x && $0.y == y }) {
            let isMe = p.nodeId == state.me.nodeId
            let controller = (isMe && state.controlling != nil) || (!isMe && state.controlledBy == p.name)
            let target = !isMe && state.controlling == p.name
            Button { selected = p } label: {
                VStack(spacing: 4) {
                    Text(p.name).font(.footnote.weight(.semibold)).multilineTextAlignment(.center).lineLimit(2)
                    Text(status(of: p)).font(.caption2).foregroundStyle(.secondary)
                }
                .frame(width: 112, height: 78)
                .background(controller ? Color.orange.opacity(0.18) : (isMe ? Color.accentColor.opacity(0.12) : Color(.secondarySystemBackground)))
                .overlay(RoundedRectangle(cornerRadius: 12).stroke(controller ? Color.orange : (target ? Color.accentColor : Color(.separator)), lineWidth: controller || target ? 3 : 1))
                .clipShape(RoundedRectangle(cornerRadius: 12))
            }
            .buttonStyle(.plain)
        } else {
            RoundedRectangle(cornerRadius: 12)
                .strokeBorder(Color(.separator), style: StrokeStyle(lineWidth: 1, dash: [4]))
                .frame(width: 112, height: 78)
        }
    }

    private var legend: some View {
        HStack(spacing: 14) {
            Label("controlling", systemImage: "square.fill").foregroundStyle(.orange)
            Label("controlled", systemImage: "square").foregroundStyle(Color.accentColor)
        }
        .font(.caption)
    }

    private func peer(for p: Placement) -> Peer? {
        state.peers.first(where: { $0.nodeId == p.nodeId })
    }

    private func status(of p: Placement) -> String {
        if p.nodeId == state.me.nodeId { return state.controlling != nil ? "controlling now" : "this phone" }
        if state.controlling == p.name { return "being controlled" }
        if state.controlledBy == p.name { return "controlling" }
        guard let peer = peer(for: p) else { return "not reachable" }
        if peer.os == "Ios" { return "iPhone (remote only)" }
        if !peer.trusted { return "not paired" }
        return peer.connected ? "online" : "offline"
    }
}
