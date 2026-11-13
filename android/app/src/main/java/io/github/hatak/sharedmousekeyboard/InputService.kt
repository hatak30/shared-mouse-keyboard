package io.github.hatak.sharedmousekeyboard

import android.accessibilityservice.AccessibilityService
import android.accessibilityservice.GestureDescription
import android.content.Context
import android.graphics.Bitmap
import android.graphics.Canvas
import android.graphics.Color
import android.graphics.Paint
import android.graphics.Path
import android.graphics.PixelFormat
import android.os.Bundle
import android.os.Handler
import android.os.Looper
import android.view.Display
import android.view.Gravity
import android.view.View
import android.view.ViewConfiguration
import android.view.WindowManager
import android.view.accessibility.AccessibilityEvent
import android.view.accessibility.AccessibilityNodeInfo
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.cancel
import kotlinx.coroutines.flow.map
import kotlinx.coroutines.launch
import kotlin.math.abs
import kotlin.math.hypot

/**
 * Replays mouse and keyboard events from other computers on this device.
 * Android has no pointer injection without root, so the mouse becomes touch
 * gestures: a click is a tap, a drag is a swipe made when the button is
 * released, the wheel is a swipe, and typing edits the focused text field.
 */
class InputService : AccessibilityService() {
    private val main = Handler(Looper.getMainLooper())
    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.Main)
    private lateinit var windowManager: WindowManager
    private var cursor: CursorView? = null
    private var cursorParams: WindowManager.LayoutParams? = null

    private var x = 0.0
    private var y = 0.0
    private var leftDown = false
    private var downX = 0.0
    private var downY = 0.0
    private var downAt = 0L
    private val trail = ArrayList<Pair<Float, Float>>()

    private var scrollX = 0
    private var scrollY = 0
    private var scrollPending = false

    private val modifiers = HashSet<Int>()
    private var metaUsed = false

    override fun onServiceConnected() {
        instance = this
        windowManager = getSystemService(Context.WINDOW_SERVICE) as WindowManager
        scope.launch {
            Store.state.map { it?.controlledBy != null }.collect { setCursorVisible(it) }
        }
        Store.command("refresh_permissions", "", "")
    }

    override fun onAccessibilityEvent(event: AccessibilityEvent?) = Unit

    override fun onInterrupt() = Unit

    override fun onUnbind(intent: android.content.Intent?): Boolean {
        release()
        return super.onUnbind(intent)
    }

    override fun onDestroy() {
        release()
        super.onDestroy()
    }

    private fun release() {
        if (instance === this) instance = null
        scope.cancel()
        setCursorVisible(false)
        Store.command("refresh_permissions", "", "")
    }

    /** Called from the engine thread. */
    fun inject(kind: Int, px: Double, py: Double, a: Int, b: Int): Boolean {
        main.post {
            when (kind) {
                0 -> moveTo(px, py)
                1 -> button(a, b != 0)
                2 -> scroll(a, b)
                3 -> key(a, b != 0)
            }
        }
        return true
    }

    // ---- snapshot

    fun takeSnapshot(): Boolean {
        if (android.os.Build.VERSION.SDK_INT < 30) return false
        takeScreenshot(Display.DEFAULT_DISPLAY, mainExecutor, object : TakeScreenshotCallback {
            override fun onSuccess(result: ScreenshotResult) {
                Thread {
                    val bytes = runCatching {
                        result.hardwareBuffer.use { buffer ->
                            val bitmap = Bitmap.wrapHardwareBuffer(buffer, result.colorSpace) ?: error("no bitmap")
                            java.io.ByteArrayOutputStream().also { bitmap.compress(Bitmap.CompressFormat.PNG, 100, it) }.toByteArray()
                        }
                    }
                    Native.snapshot(bytes.getOrNull(), bytes.exceptionOrNull()?.message.orEmpty())
                }.start()
            }

            override fun onFailure(errorCode: Int) = Native.snapshot(null, "Android refused the screenshot (code $errorCode)")
        })
        return true
    }

    // ---- pointer

    private fun screenW() = Host.pixelWidth().toDouble()
    private fun screenH() = Host.pixelHeight().toDouble()

    /** [px] and [py] arrive in dp. */
    private fun moveTo(px: Double, py: Double) {
        val density = Host.density()
        x = (px * density).coerceIn(0.0, screenW() - 1)
        y = (py * density).coerceIn(0.0, screenH() - 1)
        if (leftDown) {
            val last = trail.lastOrNull()
            if (last == null || hypot(x - last.first, y - last.second) > 12.0) trail.add(x.toFloat() to y.toFloat())
        }
        cursorParams?.let {
            it.x = x.toInt()
            it.y = y.toInt()
            cursor?.let { v -> if (v.isAttachedToWindow) windowManager.updateViewLayout(v, it) }
        }
    }

    private fun button(index: Int, down: Boolean) {
        when (index) {
            0 -> if (down) {
                leftDown = true
                downX = x
                downY = y
                downAt = System.currentTimeMillis()
                trail.clear()
                trail.add(x.toFloat() to y.toFloat())
            } else if (leftDown) {
                leftDown = false
                finishLeftPress()
            }
            1, 3 -> if (down) performGlobalAction(GLOBAL_ACTION_BACK)
            2 -> if (down) performGlobalAction(GLOBAL_ACTION_HOME)
            4 -> if (down) performGlobalAction(GLOBAL_ACTION_RECENTS)
        }
    }

    private fun finishLeftPress() {
        val held = System.currentTimeMillis() - downAt
        val slop = ViewConfiguration.get(this).scaledTouchSlop * 2
        if (hypot(x - downX, y - downY) < slop) {
            val long = held >= ViewConfiguration.getLongPressTimeout()
            gesture(listOf(downX.toFloat() to downY.toFloat()), if (long) held.coerceAtMost(2000) else 40)
        } else {
            trail.add(x.toFloat() to y.toFloat())
            gesture(trail.toList(), held.coerceIn(120, 2500))
        }
        trail.clear()
    }

    private fun scroll(dx: Int, dy: Int) {
        scrollX += dx
        scrollY += dy
        if (!scrollPending) {
            scrollPending = true
            main.postDelayed(::flushScroll, 40)
        }
    }

    private fun flushScroll() {
        if (abs(scrollX) < 20 && abs(scrollY) < 20) {
            scrollPending = false
            return
        }
        // One wheel notch (120 units) moves content by about a tenth of the screen.
        val moveX = scrollX / 120.0 * screenW() * 0.1
        val moveY = scrollY / 120.0 * screenH() * 0.1
        scrollX = 0
        scrollY = 0
        // Wheel up shows earlier content, which is a finger moving down.
        val toX = (x + moveX).coerceIn(0.0, screenW() - 1).toFloat()
        val toY = (y + moveY).coerceIn(0.0, screenH() - 1).toFloat()
        gesture(listOf(x.toFloat() to y.toFloat(), toX to toY), 100)
        // A new gesture cancels the one in flight, so wait for it to finish.
        main.postDelayed(::flushScroll, 110)
    }

    private fun gesture(points: List<Pair<Float, Float>>, duration: Long) {
        val w = (screenW() - 1).toFloat()
        val h = (screenH() - 1).toFloat()
        val path = Path()
        points.take(60).forEachIndexed { i, (px, py) ->
            val cx = px.coerceIn(0f, w)
            val cy = py.coerceIn(0f, h)
            if (i == 0) path.moveTo(cx, cy) else path.lineTo(cx, cy)
        }
        val stroke = GestureDescription.StrokeDescription(path, 0, duration.coerceAtLeast(1))
        dispatchGesture(GestureDescription.Builder().addStroke(stroke).build(), null, null)
    }

    // ---- keyboard

    private fun key(code: Int, down: Boolean) {
        if (Key.isModifier(code)) {
            if (down) {
                modifiers.add(code)
                if (Key.isMeta(code)) metaUsed = false
            } else {
                modifiers.remove(code)
                if (Key.isMeta(code) && !metaUsed) performGlobalAction(GLOBAL_ACTION_HOME)
            }
            return
        }
        if (!down) return
        if (modifiers.any(Key::isMeta)) metaUsed = true
        val ctrl = modifiers.any(Key::isCtrl) || modifiers.any(Key::isMeta)
        val shift = modifiers.any(Key::isShift)
        if (code == Key.ESC) {
            performGlobalAction(GLOBAL_ACTION_BACK)
            return
        }
        val node = rootInActiveWindow?.findFocus(AccessibilityNodeInfo.FOCUS_INPUT)?.takeIf { it.isEditable } ?: return
        if (ctrl) {
            when (code) {
                Key.A -> setSelection(node, 0, text(node).length)
                Key.C -> node.performAction(AccessibilityNodeInfo.ACTION_COPY)
                Key.X -> node.performAction(AccessibilityNodeInfo.ACTION_CUT)
                Key.V -> node.performAction(AccessibilityNodeInfo.ACTION_PASTE)
            }
            return
        }
        when (code) {
            Key.BACKSPACE -> edit(node) { t, s, e -> if (s != e) replace(t, s, e, "") else if (s > 0) replace(t, s - 1, e, "") else t to s }
            Key.DELETE -> edit(node) { t, s, e -> if (s != e) replace(t, s, e, "") else if (e < t.length) replace(t, s, e + 1, "") else t to s }
            Key.LEFT -> moveCaret(node, -1)
            Key.RIGHT -> moveCaret(node, 1)
            Key.HOME -> setSelection(node, 0, 0)
            Key.END -> text(node).length.let { setSelection(node, it, it) }
            Key.ENTER, Key.KPENTER -> enter(node)
            Key.TAB -> insert(node, "\t")
            else -> KeyMap.charFor(code, shift)?.let { insert(node, it.toString()) }
        }
    }

    private fun text(node: AccessibilityNodeInfo): String =
        if (node.isShowingHintText) "" else node.text?.toString().orEmpty()

    private fun selection(node: AccessibilityNodeInfo, length: Int): Pair<Int, Int> {
        val s = node.textSelectionStart
        val e = node.textSelectionEnd
        return if (s < 0 || e < 0) length to length else minOf(s, e) to maxOf(s, e)
    }

    private fun replace(t: String, s: Int, e: Int, with: String) = (t.substring(0, s) + with + t.substring(e)) to (s + with.length)

    private fun edit(node: AccessibilityNodeInfo, change: (String, Int, Int) -> Pair<String, Int>) {
        val t = text(node)
        val (s, e) = selection(node, t.length)
        val (newText, caret) = change(t, s.coerceIn(0, t.length), e.coerceIn(0, t.length))
        if (newText != t) {
            val args = Bundle().apply { putCharSequence(AccessibilityNodeInfo.ACTION_ARGUMENT_SET_TEXT_CHARSEQUENCE, newText) }
            node.performAction(AccessibilityNodeInfo.ACTION_SET_TEXT, args)
        }
        setSelection(node, caret, caret)
    }

    private fun insert(node: AccessibilityNodeInfo, s: String) = edit(node) { t, a, b -> replace(t, a, b, s) }

    private fun moveCaret(node: AccessibilityNodeInfo, delta: Int) {
        val t = text(node)
        val (s, e) = selection(node, t.length)
        val pos = (if (delta < 0) s else e) + if (s != e) 0 else delta
        pos.coerceIn(0, t.length).let { setSelection(node, it, it) }
    }

    private fun setSelection(node: AccessibilityNodeInfo, start: Int, end: Int) {
        val args = Bundle().apply {
            putInt(AccessibilityNodeInfo.ACTION_ARGUMENT_SELECTION_START_INT, start)
            putInt(AccessibilityNodeInfo.ACTION_ARGUMENT_SELECTION_END_INT, end)
        }
        node.performAction(AccessibilityNodeInfo.ACTION_SET_SELECTION, args)
    }

    private fun enter(node: AccessibilityNodeInfo) {
        if (node.isMultiLine) {
            insert(node, "\n")
        } else if (android.os.Build.VERSION.SDK_INT >= 30) {
            node.performAction(AccessibilityNodeInfo.AccessibilityAction.ACTION_IME_ENTER.id)
        }
    }

    // ---- cursor overlay

    private fun setCursorVisible(visible: Boolean) {
        if (!visible) {
            cursor?.let { if (it.isAttachedToWindow) windowManager.removeView(it) }
            cursor = null
            cursorParams = null
            return
        }
        if (cursor != null) return
        val view = CursorView(this)
        val startX = x.toInt()
        val startY = y.toInt()
        val params = WindowManager.LayoutParams(
            WindowManager.LayoutParams.WRAP_CONTENT,
            WindowManager.LayoutParams.WRAP_CONTENT,
            WindowManager.LayoutParams.TYPE_ACCESSIBILITY_OVERLAY,
            WindowManager.LayoutParams.FLAG_NOT_FOCUSABLE or
                WindowManager.LayoutParams.FLAG_NOT_TOUCHABLE or
                WindowManager.LayoutParams.FLAG_LAYOUT_NO_LIMITS or
                WindowManager.LayoutParams.FLAG_LAYOUT_IN_SCREEN,
            PixelFormat.TRANSLUCENT,
        ).apply {
            gravity = Gravity.TOP or Gravity.START
            this.x = startX
            this.y = startY
        }
        windowManager.addView(view, params)
        cursor = view
        cursorParams = params
    }

    private class CursorView(context: Context) : View(context) {
        private val density = resources.displayMetrics.density
        private val fill = Paint(Paint.ANTI_ALIAS_FLAG).apply { color = Color.WHITE }
        private val outline = Paint(Paint.ANTI_ALIAS_FLAG).apply {
            color = Color.BLACK
            style = Paint.Style.STROKE
            strokeWidth = 1.5f
        }
        private val arrow = Path().apply {
            moveTo(0f, 0f)
            lineTo(0f, 17f)
            lineTo(4.5f, 13f)
            lineTo(7.5f, 20f)
            lineTo(10.5f, 18.5f)
            lineTo(7.5f, 12f)
            lineTo(13.5f, 12f)
            close()
        }

        override fun onMeasure(widthMeasureSpec: Int, heightMeasureSpec: Int) {
            setMeasuredDimension((16 * density).toInt(), (22 * density).toInt())
        }

        override fun onDraw(canvas: Canvas) {
            canvas.scale(density, density)
            canvas.drawPath(arrow, fill)
            canvas.drawPath(arrow, outline)
        }
    }

    companion object {
        @Volatile var instance: InputService? = null
            private set
    }
}
