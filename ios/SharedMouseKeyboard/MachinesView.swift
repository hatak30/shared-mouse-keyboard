import SwiftUI

struct MachinesView: View {
    @EnvironmentObject var model: Model
    let state: UiState
    @State private var address = ""

    var body: some View {
        NavigationStack {
            List {
                Section("This device") {
                    VStack(alignment: .leading, spacing: 4) {
                        Text(state.me.name).font(.headline)
                        Text("\(osLabel(state.me.os)) · port \(state.me.port)").font(.subheadline).foregroundStyle(.secondary)
                        Text("iOS cannot be controlled by other computers. Use this phone as a touchpad and keyboard for the machines below.")
                            .font(.footnote).foregroundStyle(.secondary)
                    }
                }
                Section("Machines") {
                    if state.peers.isEmpty {
                        Text("No machines found yet. Start the app on a computer on the same Wi-Fi, or add one by address.")
                            .foregroundStyle(.secondary)
                    }
                    ForEach(state.peers) { PeerRow(peer: $0) }
                }
                Section("Add by address") {
                    TextField("IP address or host name", text: $address)
                        .textInputAutocapitalization(.never)
                        .autocorrectionDisabled()
                    Button("Connect") {
                        model.send("connect", address.trimmingCharacters(in: .whitespaces))
                        address = ""
                    }
                    .disabled(address.trimmingCharacters(in: .whitespaces).isEmpty)
                    ForEach(state.manualPeers, id: \.self) { addr in
                        HStack {
                            Text(addr)
                            Spacer()
                            Button("Remove", role: .destructive) { model.send("remove_manual_peer", addr) }
                                .buttonStyle(.borderless)
                        }
                    }
                }
                if !state.log.isEmpty {
                    Section("Activity") {
                        ForEach(Array(state.log.suffix(8).reversed().enumerated()), id: \.offset) { _, entry in
                            Text(entry.text).font(.footnote)
                        }
                    }
                }
            }
            .navigationTitle("Shared Mouse Keyboard")
        }
    }
}

struct PeerRow: View {
    @EnvironmentObject var model: Model
    let peer: Peer
    @State private var code = ""

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            HStack {
                Text(peer.name).font(.headline)
                Spacer()
                Text(peer.trusted ? (peer.connected ? "Connected" : "Offline") : "Not paired")
                    .font(.caption)
                    .foregroundStyle(peer.trusted && peer.connected ? Color.accentColor : .secondary)
            }
            Text([osLabel(peer.os), peer.addr].compactMap { $0 }.joined(separator: " · "))
                .font(.subheadline).foregroundStyle(.secondary)

            if let pairing = peer.pairing {
                switch pairing.kind {
                case "show_code":
                    Text("Enter this code on \(peer.name):")
                    Text(pairing.code ?? "").font(.system(size: 34, weight: .semibold, design: .monospaced))
                    Button("Cancel") { model.send("cancel_pair", peer.nodeId) }.buttonStyle(.borderless)
                case "enter_code":
                    Text("Type the 6-digit code shown on \(peer.name):")
                    TextField("123456", text: $code)
                        .keyboardType(.numberPad)
                        .onChange(of: code) { code = String($0.filter(\.isNumber).prefix(6)) }
                    if let error = pairing.error { Text(error).foregroundStyle(.red).font(.footnote) }
                    HStack {
                        Button("Pair") { model.send("submit_code", peer.nodeId, code) }
                            .buttonStyle(.borderedProminent).disabled(code.count != 6)
                        Button("Cancel") { model.send("cancel_pair", peer.nodeId) }.buttonStyle(.borderless)
                    }
                default:
                    Text("\(peer.name) does not accept pairing requests.").foregroundStyle(.red)
                    Button("OK") { model.send("cancel_pair", peer.nodeId) }.buttonStyle(.borderless)
                }
            } else if peer.trusted {
                HStack {
                    if peer.connected {
                        Button("Control") { model.send("control_peer", peer.nodeId) }.buttonStyle(.borderedProminent)
                    }
                    Button("Unpair", role: .destructive) { model.send("unpair", peer.nodeId) }.buttonStyle(.bordered)
                }
            } else {
                HStack {
                    Button("Pair") { model.send("pair", peer.nodeId) }.buttonStyle(.borderedProminent)
                    Button("Block", role: .destructive) { model.send("block", peer.nodeId) }.buttonStyle(.bordered)
                }
            }
        }
        .padding(.vertical, 4)
    }
}
