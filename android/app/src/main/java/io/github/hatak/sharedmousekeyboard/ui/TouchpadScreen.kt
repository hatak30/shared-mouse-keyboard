package io.github.hatak.sharedmousekeyboard.ui

import androidx.activity.compose.BackHandler
import androidx.compose.foundation.gestures.awaitEachGesture
import androidx.compose.foundation.gestures.awaitFirstDown
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.imePadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.safeDrawingPadding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.BasicTextField
import androidx.compose.foundation.text.KeyboardActions
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.material3.Button
import androidx.compose.material3.FilledTonalButton
import androidx.compose.material3.FilterChip
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.focus.FocusRequester
import androidx.compose.ui.focus.focusRequester
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.input.pointer.positionChange
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.text.TextRange
import androidx.compose.ui.text.input.ImeAction
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.text.input.TextFieldValue
import androidx.compose.ui.unit.dp
import io.github.hatak.sharedmousekeyboard.Key
import io.github.hatak.sharedmousekeyboard.KeyMap
import io.github.hatak.sharedmousekeyboard.Native
import io.github.hatak.sharedmousekeyboard.UiState
import kotlin.math.abs

private const val POINTER_GAIN = 1.4
private const val SCROLL_UNITS_PER_DP = 5.0
private const val LEFT = 0.0
private const val RIGHT = 1.0

private fun click(button: Double) {
    Native.input(1, button, 1.0)
    Native.input(1, button, 0.0)
}

private fun key(code: Int, down: Boolean) = Native.input(3, code.toDouble(), if (down) 1.0 else 0.0)

private fun tapKey(code: Int, modifiers: Collection<Int>) {
    modifiers.forEach { key(it, true) }
    key(code, true)
    key(code, false)
    modifiers.reversed().forEach { key(it, false) }
}

private val stickyKeys = listOf("Ctrl" to Key.LEFTCTRL, "Alt" to Key.LEFTALT, "Shift" to Key.LEFTSHIFT, "Win" to Key.LEFTMETA)
private val extraKeys = listOf(
    "Esc" to Key.ESC, "Tab" to Key.TAB, "←" to Key.LEFT, "↑" to Key.UP, "↓" to Key.DOWN, "→" to Key.RIGHT,
    "Home" to Key.HOME, "End" to Key.END, "Del" to Key.DELETE,
)

@Composable
fun TouchpadScreen(s: UiState) {
    BackHandler { send("return_home") }
    var keyboard by remember { mutableStateOf(false) }
    var dragLock by remember { mutableStateOf(false) }
    var sticky by remember { mutableStateOf(setOf<Int>()) }

    fun press(code: Int, extraShift: Boolean = false) {
        val mods = sticky.toMutableList()
        if (extraShift && Key.LEFTSHIFT !in mods) mods.add(Key.LEFTSHIFT)
        tapKey(code, mods)
        sticky = emptySet()
    }

    Column(Modifier.fillMaxSize().safeDrawingPadding().imePadding().padding(12.dp), verticalArrangement = Arrangement.spacedBy(12.dp)) {
        Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.SpaceBetween, verticalAlignment = Alignment.CenterVertically) {
            Column {
                Text("Controlling", style = MaterialTheme.typography.labelMedium)
                Text(s.controlling.orEmpty(), style = MaterialTheme.typography.titleLarge)
            }
            Button(onClick = { send("return_home") }) { Text("Return") }
        }

        Surface(
            modifier = Modifier.weight(1f).fillMaxWidth().touchpad(),
            shape = RoundedCornerShape(24.dp),
            color = MaterialTheme.colorScheme.surfaceVariant,
        ) {
            Box(contentAlignment = Alignment.Center) {
                Text(
                    "Drag to move · tap to click · two fingers to scroll or right-click",
                    style = MaterialTheme.typography.bodyMedium,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                    modifier = Modifier.padding(24.dp),
                )
            }
        }

        Row(horizontalArrangement = Arrangement.spacedBy(8.dp), modifier = Modifier.fillMaxWidth()) {
            FilledTonalButton(onClick = { click(LEFT) }, modifier = Modifier.weight(1f)) { Text("Left click") }
            FilledTonalButton(onClick = { click(RIGHT) }, modifier = Modifier.weight(1f)) { Text("Right click") }
        }
        Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
            FilterChip(
                selected = dragLock,
                onClick = {
                    dragLock = !dragLock
                    Native.input(1, LEFT, if (dragLock) 1.0 else 0.0)
                },
                label = { Text("Hold left button") },
            )
            FilterChip(selected = keyboard, onClick = { keyboard = !keyboard }, label = { Text("Keyboard") })
        }

        if (keyboard) {
            Row(Modifier.horizontalScroll(rememberScrollState()), horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                stickyKeys.forEach { (label, code) ->
                    FilterChip(
                        selected = code in sticky,
                        onClick = { sticky = if (code in sticky) sticky - code else sticky + code },
                        label = { Text(label) },
                    )
                }
            }
            Row(Modifier.horizontalScroll(rememberScrollState()), horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                extraKeys.forEach { (label, code) ->
                    FilledTonalButton(onClick = { press(code) }) { Text(label) }
                }
            }
            KeyboardCapture(
                onChar = { c -> KeyMap.keyFor(c)?.let { (code, shift) -> press(code, shift) } },
                onBackspace = { press(Key.BACKSPACE) },
                onEnter = { press(Key.ENTER) },
            )
        }
    }
}

private fun Modifier.touchpad(): Modifier = pointerInput(Unit) {
    val density = this.density
    val clickSlop = 12 * density
    val longPressMs = 500L
    awaitEachGesture {
        val first = awaitFirstDown(requireUnconsumed = false)
        val startedAt = first.uptimeMillis
        var lastAt = startedAt
        var fingers = 1
        var travelled = 0f
        var remX = 0.0
        var remY = 0.0
        do {
            val event = awaitPointerEvent()
            val pressed = event.changes.filter { it.pressed }
            fingers = maxOf(fingers, pressed.size)
            lastAt = event.changes.maxOf { it.uptimeMillis }
            if (pressed.size >= 2) {
                val avg = pressed.map { it.positionChange() }.let { moves ->
                    Offset(moves.map { it.x }.average().toFloat(), moves.map { it.y }.average().toFloat())
                }
                travelled += abs(avg.x) + abs(avg.y)
                // Content follows the fingers, like on a phone.
                remY += avg.y / density * SCROLL_UNITS_PER_DP
                remX -= avg.x / density * SCROLL_UNITS_PER_DP
                val sx = remX.toInt()
                val sy = remY.toInt()
                if (sx != 0 || sy != 0) {
                    Native.input(2, sx.toDouble(), sy.toDouble())
                    remX -= sx
                    remY -= sy
                }
            } else if (pressed.size == 1) {
                val move = pressed[0].positionChange()
                travelled += abs(move.x) + abs(move.y)
                if (move != Offset.Zero) Native.input(0, move.x * POINTER_GAIN, move.y * POINTER_GAIN)
            }
            event.changes.forEach { it.consume() }
        } while (event.changes.any { it.pressed })

        if (travelled < clickSlop) {
            val held = lastAt - startedAt
            click(if (fingers >= 2 || held >= longPressMs) RIGHT else LEFT)
        }
    }
}

private const val SENTINEL = " "

/** An invisible text field whose only job is to bring up the on-screen keyboard and report what was typed. */
@Composable
private fun KeyboardCapture(onChar: (Char) -> Unit, onBackspace: () -> Unit, onEnter: () -> Unit) {
    val focus = remember { FocusRequester() }
    var value by remember { mutableStateOf(TextFieldValue(SENTINEL, TextRange(SENTINEL.length))) }
    BasicTextField(
        value = value,
        onValueChange = { new ->
            when {
                new.text.length > SENTINEL.length -> new.text.drop(SENTINEL.length).forEach(onChar)
                new.text.length < SENTINEL.length -> onBackspace()
            }
            value = TextFieldValue(SENTINEL, TextRange(SENTINEL.length))
        },
        modifier = Modifier.size(1.dp).focusRequester(focus),
        keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Password, imeAction = ImeAction.Send, autoCorrectEnabled = false),
        keyboardActions = KeyboardActions(onSend = { onEnter() }),
    )
    LaunchedEffect(Unit) { focus.requestFocus() }
}
