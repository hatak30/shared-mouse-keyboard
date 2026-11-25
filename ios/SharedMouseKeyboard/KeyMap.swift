import Foundation

/// Linux evdev key codes (the wire format) for a US layout.
enum Key {
    static let esc = 1, backspace = 14, tab = 15, enter = 28
    static let ctrl = 29, shift = 42, alt = 56, space = 57
    static let home = 102, up = 103, left = 105, right = 106, end = 107, down = 108
    static let delete = 111, meta = 125
}

enum KeyMap {
    private static let table: [Character: (code: Int, shift: Bool)] = {
        var map: [Character: (Int, Bool)] = [:]
        func row(_ first: Int, _ plain: String, _ shifted: String) {
            for (i, (a, b)) in zip(plain, shifted).enumerated() {
                map[a] = map[a] ?? (first + i, false)
                map[b] = map[b] ?? (first + i, true)
            }
        }
        row(2, "1234567890-=", "!@#$%^&*()_+")
        row(16, "qwertyuiop[]", "QWERTYUIOP{}")
        row(30, "asdfghjkl;'", "ASDFGHJKL:\"")
        row(41, "`", "~")
        row(43, "\\", "|")
        row(44, "zxcvbnm,./", "ZXCVBNM<>?")
        map[" "] = (Key.space, false)
        return map
    }()

    static func key(for c: Character) -> (code: Int, shift: Bool)? { table[c] }
}
