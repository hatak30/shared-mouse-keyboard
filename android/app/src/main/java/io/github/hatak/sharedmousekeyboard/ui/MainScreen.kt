package io.github.hatak.sharedmousekeyboard.ui

import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.padding
import androidx.compose.material3.NavigationBar
import androidx.compose.material3.NavigationBarItem
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import io.github.hatak.sharedmousekeyboard.UiState

private val tabs = listOf("Machines" to "🖥", "Layout" to "▦", "Files" to "⇩", "Settings" to "⚙")

@Composable
fun MainScreen(s: UiState) {
    var tab by rememberSaveable { mutableIntStateOf(0) }
    Scaffold(
        bottomBar = {
            NavigationBar {
                tabs.forEachIndexed { i, (label, icon) ->
                    NavigationBarItem(
                        selected = tab == i,
                        onClick = { tab = i },
                        icon = { Text(icon) },
                        label = { Text(label) },
                    )
                }
            }
        },
    ) { padding ->
        PairingDialog(s)
        Box(Modifier.padding(padding)) {
            when (tab) {
                0 -> MachinesTab(s)
                1 -> LayoutTab(s)
                2 -> TransfersTab(s)
                else -> SettingsTab(s)
            }
        }
    }
}
