package io.github.hatak.sharedmousekeyboard

object Native {
    init {
        System.loadLibrary("smk_android")
    }

    @JvmStatic external fun start(host: Any, configDir: String, downloadDir: String): Boolean

    @JvmStatic external fun state(): String

    /** Returns an error message, or an empty string. */
    @JvmStatic external fun command(name: String, a: String, b: String): String

    /** kind: 0 motion(dx, dy), 1 button(index, down), 2 scroll(dx, dy), 3 key(evdev code, down). */
    @JvmStatic external fun input(kind: Int, a: Double, b: Double)

    /** Puts text on the clipboard of the paired computers. */
    @JvmStatic external fun shareText(text: String)

    @JvmStatic external fun clipboardChanged()

    /** The screenshot asked for by [Host.captureScreen]: PNG bytes, or null with an error. */
    @JvmStatic external fun snapshot(png: ByteArray?, error: String)
}
