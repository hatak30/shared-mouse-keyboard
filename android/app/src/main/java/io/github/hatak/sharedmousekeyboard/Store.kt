package io.github.hatak.sharedmousekeyboard

import android.util.Log
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow

object Store {
    private val _state = MutableStateFlow<UiState?>(null)
    val state: StateFlow<UiState?> = _state

    fun refresh() {
        try {
            val json = Native.state()
            if (json.isNotEmpty()) _state.value = parseState(json)
        } catch (e: Exception) {
            Log.e("smk", "bad engine state", e)
        }
    }

    fun command(name: String, a: String = "", b: String = "") {
        val error = Native.command(name, a, b)
        if (error.isNotEmpty()) Log.w("smk", "$name: $error")
    }
}
