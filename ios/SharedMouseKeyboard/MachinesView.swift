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
                        Text("\(osLabel(state.me.os)) · port \(String(state.me.port))").font(.subheadline).foregroundStyle(.secondary)
                        Text("Address: " + (localAddresses().joined(separator: ", ").isEmpty ? "not on Wi-Fi" : localAddresses().joined(separator: ", ")))
                            .font(.footnote).foregroundStyle(.secondary)
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

            if peer.pairing != nil {
                Text("Pairing… follow the dialog").font(.footnote).foregroundStyle(.secondary)
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

/// Pairing dialog: the machine that was asked shows a code, the other types it.
struct PairingSheet: View {
    @EnvironmentObject var model: Model
    let peer: Peer
    @State private var code = ""
    @FocusState private var focused: Bool

    var body: some View {
        NavigationStack {
            VStack(spacing: 20) {
                if let pairing = peer.pairing {
                    switch pairing.kind {
                    case "show_code":
                        Text("\(peer.name) wants to pair").font(.headline)
                        Text("Enter this code on \(peer.name):").foregroundStyle(.secondary)
                        Text(pairing.code ?? "")
                            .font(.system(size: 56, weight: .bold, design: .monospaced))
                            .padding(.vertical, 8)
                    case "enter_code":
                        Text("Pair with \(peer.name)").font(.headline)
                        Text("Type the 6-digit code shown on \(peer.name).").foregroundStyle(.secondary)
                        TextField("000000", text: $code)
                            .keyboardType(.numberPad)
                            .font(.system(size: 40, weight: .bold, design: .monospaced))
                            .multilineTextAlignment(.center)
                            .textFieldStyle(.roundedBorder)
                            .focused($focused)
                            .onChange(of: code) { code = String($0.filter(\.isNumber).prefix(6)) }
                        if let error = pairing.error {
                            Text(error).foregroundStyle(.red).font(.footnote)
                        }
                        Button("Pair") { model.send("submit_code", peer.nodeId, code) }
                            .buttonStyle(.borderedProminent)
                            .controlSize(.large)
                            .disabled(code.count != 6)
                    default:
                        Text("\(peer.name) does not accept pairing requests.").foregroundStyle(.red)
                    }
                }
                Spacer()
            }
            .padding()
            .navigationTitle("Pairing")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("Cancel") { model.send("cancel_pair", peer.nodeId) }
                }
            }
            .onAppear { focused = true }
        }
        .interactiveDismissDisabled()
    }
}
