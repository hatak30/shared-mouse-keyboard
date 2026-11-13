package io.github.hatak.sharedmousekeyboard

/** Linux evdev key codes (the wire format) and the US-layout characters they produce. */
object Key {
    const val ESC = 1
    const val BACKSPACE = 14
    const val TAB = 15
    const val ENTER = 28
    const val LEFTCTRL = 29
    const val LEFTSHIFT = 42
    const val RIGHTSHIFT = 54
    const val LEFTALT = 56
    const val SPACE = 57
    const val KPENTER = 96
    const val RIGHTCTRL = 97
    const val RIGHTALT = 100
    const val HOME = 102
    const val UP = 103
    const val LEFT = 105
    const val RIGHT = 106
    const val END = 107
    const val DOWN = 108
    const val DELETE = 111
    const val LEFTMETA = 125
    const val RIGHTMETA = 126

    const val A = 30
    const val C = 46
    const val V = 47
    const val X = 45

    fun isCtrl(code: Int) = code == LEFTCTRL || code == RIGHTCTRL
    fun isShift(code: Int) = code == LEFTSHIFT || code == RIGHTSHIFT
    fun isAlt(code: Int) = code == LEFTALT || code == RIGHTALT
    fun isMeta(code: Int) = code == LEFTMETA || code == RIGHTMETA
    fun isModifier(code: Int) = isCtrl(code) || isShift(code) || isAlt(code) || isMeta(code)
}

object KeyMap {
    private val plain = HashMap<Int, Char>()
    private val shifted = HashMap<Int, Char>()
    private val reverse = HashMap<Char, Pair<Int, Boolean>>()

    private fun row(first: Int, normal: String, withShift: String) {
        normal.forEachIndexed { i, c -> add(first + i, c, withShift[i]) }
    }

    private fun add(code: Int, normal: Char, withShift: Char) {
        plain[code] = normal
        shifted[code] = withShift
        reverse.putIfAbsent(normal, code to false)
        reverse.putIfAbsent(withShift, code to true)
    }

    init {
        row(2, "1234567890-=", "!@#$%^&*()_+")
        row(16, "qwertyuiop[]", "QWERTYUIOP{}")
        row(30, "asdfghjkl;'", "ASDFGHJKL:\"")
        add(41, '`', '~')
        add(43, '\\', '|')
        row(44, "zxcvbnm,./", "ZXCVBNM<>?")
        add(Key.SPACE, ' ', ' ')
        // Keypad digits are typed like the number row.
        "789".forEachIndexed { i, c -> add(71 + i, c, c) }
        "456".forEachIndexed { i, c -> add(75 + i, c, c) }
        "123".forEachIndexed { i, c -> add(79 + i, c, c) }
        add(82, '0', '0')
    }

    fun charFor(code: Int, shift: Boolean): Char? = if (shift) shifted[code] else plain[code]

    /** The key (and whether Shift must be held) that types [c] on a US layout. */
    fun keyFor(c: Char): Pair<Int, Boolean>? = reverse[c]
}
