import SwiftUI
import UIKit

struct TouchpadView: View {
    @EnvironmentObject var model: Model
    let state: UiState
    @State private var keyboard = false
    @State private var dragLock = false
    @State private var sticky: Set<Int> = []

    private let stickyKeys: [(String, Int)] = [("Ctrl", Key.ctrl), ("Alt", Key.alt), ("Shift", Key.shift), ("Win", Key.meta)]
    private let extraKeys: [(String, Int)] = [
        ("Esc", Key.esc), ("Tab", Key.tab), ("←", Key.left), ("↑", Key.up), ("↓", Key.down), ("→", Key.right),
        ("Home", Key.home), ("End", Key.end), ("Del", Key.delete),
    ]

    var body: some View {
        VStack(spacing: 12) {
            HStack {
                VStack(alignment: .leading) {
                    Text("Controlling").font(.caption).foregroundStyle(.secondary)
                    Text(state.controlling ?? "").font(.title2.bold())
                }
                Spacer()
                Button("Return") { model.send("return_home") }.buttonStyle(.borderedProminent)
            }
            ZStack {
                TouchpadSurface(model: model)
                Text("Drag to move · tap to click\ntwo fingers to scroll or right-click")
                    .multilineTextAlignment(.center)
                    .font(.footnote).foregroundStyle(.secondary)
                    .allowsHitTesting(false)
            }
            .background(Color(.secondarySystemBackground))
            .clipShape(RoundedRectangle(cornerRadius: 24))

            HStack {
                Button("Left click") { click(0) }.buttonStyle(.bordered).frame(maxWidth: .infinity)
                Button("Right click") { click(1) }.buttonStyle(.bordered).frame(maxWidth: .infinity)
            }
            HStack {
                Toggle("Hold left button", isOn: Binding(get: { dragLock }, set: { on in
                    dragLock = on
                    model.input(1, 0, on ? 1 : 0)
                })).toggleStyle(.button)
                Toggle("Keyboard", isOn: $keyboard).toggleStyle(.button)
            }
            if keyboard {
                ScrollView(.horizontal, showsIndicators: false) {
                    HStack {
                        ForEach(stickyKeys, id: \.1) { label, code in
                            Toggle(label, isOn: Binding(get: { sticky.contains(code) }, set: { on in
                                if on { sticky.insert(code) } else { sticky.remove(code) }
                            })).toggleStyle(.button)
                        }
                        ForEach(extraKeys, id: \.1) { label, code in
                            Button(label) { press(code) }.buttonStyle(.bordered)
                        }
                    }
                }
            }
            KeyboardCapture(active: keyboard) { text in
                for c in text {
                    if text == "\n" { press(Key.enter) } else if let k = KeyMap.key(for: c) { press(k.code, shift: k.shift) }
                }
            } onBackspace: { press(Key.backspace) }
                .frame(width: 1, height: 1)
        }
        .padding(12)
    }

    private func click(_ button: Double) {
        model.input(1, button, 1)
        model.input(1, button, 0)
    }

    private func press(_ code: Int, shift: Bool = false) {
        var mods = Array(sticky).sorted()
        if shift && !mods.contains(Key.shift) { mods.append(Key.shift) }
        mods.forEach { model.input(3, Double($0), 1) }
        model.input(3, Double(code), 1)
        model.input(3, Double(code), 0)
        mods.reversed().forEach { model.input(3, Double($0), 0) }
        sticky = []
    }
}

/// Invisible responder that brings up the system keyboard and reports typed characters.
struct KeyboardCapture: UIViewRepresentable {
    var active: Bool
    var onText: (String) -> Void
    var onBackspace: () -> Void

    func makeUIView(context: Context) -> CaptureView {
        let v = CaptureView()
        v.onText = onText
        v.onBackspace = onBackspace
        return v
    }

    func updateUIView(_ v: CaptureView, context: Context) {
        v.onText = onText
        v.onBackspace = onBackspace
        DispatchQueue.main.async {
            if active { v.becomeFirstResponder() } else { v.resignFirstResponder() }
        }
    }

    final class CaptureView: UIView, UIKeyInput {
        var onText: (String) -> Void = { _ in }
        var onBackspace: () -> Void = {}
        override var canBecomeFirstResponder: Bool { true }
        var hasText: Bool { true }
        func insertText(_ text: String) { onText(text) }
        func deleteBackward() { onBackspace() }
        var autocorrectionType: UITextAutocorrectionType { get { .no } set {} }
        var autocapitalizationType: UITextAutocapitalizationType { get { .none } set {} }
        var keyboardType: UIKeyboardType { get { .asciiCapable } set {} }
    }
}

/// Multi-touch surface: one finger moves, two fingers scroll, taps click.
struct TouchpadSurface: UIViewRepresentable {
    let model: Model

    func makeCoordinator() -> Coordinator { Coordinator(model) }

    func makeUIView(context: Context) -> UIView {
        let view = UIView()
        view.backgroundColor = .clear
        let c = context.coordinator

        let move = UIPanGestureRecognizer(target: c, action: #selector(Coordinator.move(_:)))
        move.maximumNumberOfTouches = 1
        let scroll = UIPanGestureRecognizer(target: c, action: #selector(Coordinator.scroll(_:)))
        scroll.minimumNumberOfTouches = 2
        scroll.maximumNumberOfTouches = 2
        let tap = UITapGestureRecognizer(target: c, action: #selector(Coordinator.tap(_:)))
        let twoTap = UITapGestureRecognizer(target: c, action: #selector(Coordinator.twoFingerTap(_:)))
        twoTap.numberOfTouchesRequired = 2
        let long = UILongPressGestureRecognizer(target: c, action: #selector(Coordinator.longPress(_:)))
        long.minimumPressDuration = 0.5
        long.allowableMovement = 10
        [move, scroll, tap, twoTap, long].forEach(view.addGestureRecognizer)
        return view
    }

    func updateUIView(_ uiView: UIView, context: Context) {}

    final class Coordinator: NSObject {
        let model: Model
        private var remX = 0.0
        private var remY = 0.0
        private let gain = 1.4
        private let unitsPerPoint = 5.0

        init(_ model: Model) { self.model = model }

        @objc func move(_ g: UIPanGestureRecognizer) {
            let t = g.translation(in: g.view)
            g.setTranslation(.zero, in: g.view)
            model.input(0, Double(t.x) * gain, Double(t.y) * gain)
        }

        @objc func scroll(_ g: UIPanGestureRecognizer) {
            let t = g.translation(in: g.view)
            g.setTranslation(.zero, in: g.view)
            // Content follows the fingers, like on a phone.
            remY += Double(t.y) * unitsPerPoint
            remX -= Double(t.x) * unitsPerPoint
            let sx = remX.rounded(.towardZero), sy = remY.rounded(.towardZero)
            if sx != 0 || sy != 0 {
                model.input(2, sx, sy)
                remX -= sx
                remY -= sy
            }
        }

        @objc func tap(_ g: UITapGestureRecognizer) { click(0) }
        @objc func twoFingerTap(_ g: UITapGestureRecognizer) { click(1) }
        @objc func longPress(_ g: UILongPressGestureRecognizer) {
            if g.state == .began { click(1) }
        }

        private func click(_ button: Double) {
            model.input(1, button, 1)
            model.input(1, button, 0)
        }
    }
}
