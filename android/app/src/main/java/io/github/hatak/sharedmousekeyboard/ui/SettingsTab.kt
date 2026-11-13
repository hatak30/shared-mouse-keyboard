package io.github.hatak.sharedmousekeyboard.ui

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.material3.Button
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Slider
import androidx.compose.material3.Switch
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableFloatStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import io.github.hatak.sharedmousekeyboard.UiState
import io.github.hatak.sharedmousekeyboard.withSetting

@Composable
fun SettingsTab(s: UiState) {
    LazyColumn(Modifier.fillMaxSize(), contentPadding = ScreenPadding, verticalArrangement = ItemSpacing) {
        item { NameCard(s) }
        item { Toggles(s) }
        item { PointerCard(s) }
        item { PermissionsCard(s) }
        if (s.blocked.isNotEmpty()) item { BlockedCard(s) }
        item { LogCard(s) }
    }
}

@Composable
private fun NameCard(s: UiState) {
    var name by remember(s.me.name) { mutableStateOf(s.me.name) }
    InfoCard(Modifier.fillMaxWidth()) {
        SectionTitle("This device")
        OutlinedTextField(value = name, onValueChange = { name = it }, singleLine = true, label = { Text("Name shown to other machines") }, modifier = Modifier.fillMaxWidth())
        Button(enabled = name.isNotBlank() && name != s.me.name, onClick = { send("set_name", name.trim()) }) { Text("Save name") }
        Text("Fingerprint: ${s.me.fingerprint.take(23)}…", style = MaterialTheme.typography.bodySmall)
    }
}

@Composable
private fun Toggles(s: UiState) {
    fun on(key: String) = s.settings.optBoolean(key)
    InfoCard(Modifier.fillMaxWidth()) {
        SectionTitle("Sharing")
        ToggleRow("Share the clipboard (text)", on("clipboard_enabled")) { send("set_settings", s.withSetting("clipboard_enabled", it)) }
        ToggleRow("Allow other machines to send pairing requests", on("accept_pairing")) { send("set_settings", s.withSetting("accept_pairing", it)) }
        ToggleRow("Swap Command and Ctrl between Mac and other systems", on("swap_cmd_ctrl")) { send("set_settings", s.withSetting("swap_cmd_ctrl", it)) }
    }
}

@Composable
private fun ToggleRow(label: String, checked: Boolean, onChange: (Boolean) -> Unit) {
    Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.SpaceBetween, verticalAlignment = Alignment.CenterVertically) {
        Text(label, Modifier.weight(1f))
        Switch(checked = checked, onCheckedChange = onChange)
    }
}

@Composable
private fun PointerCard(s: UiState) {
    var speed by remember(s.settings.optDouble("pointer_speed", 1.0)) { mutableFloatStateOf(s.settings.optDouble("pointer_speed", 1.0).toFloat()) }
    var resistance by remember(s.settings.optDouble("edge_resistance_px", 0.0)) { mutableFloatStateOf(s.settings.optDouble("edge_resistance_px", 0.0).toFloat()) }
    InfoCard(Modifier.fillMaxWidth()) {
        SectionTitle("Pointer")
        Text("Speed when this device controls another machine: ${"%.1f".format(speed)}×")
        Slider(value = speed, onValueChange = { speed = it }, valueRange = 0.5f..3f, onValueChangeFinished = {
            send("set_settings", s.withSetting("pointer_speed", speed.toDouble()))
        })
        Text("Edge resistance: ${resistance.toInt()} px")
        Slider(value = resistance, onValueChange = { resistance = it }, valueRange = 0f..200f, onValueChangeFinished = {
            send("set_settings", s.withSetting("edge_resistance_px", resistance.toDouble()))
        })
    }
}

@Composable
private fun PermissionsCard(s: UiState) {
    InfoCard(Modifier.fillMaxWidth()) {
        SectionTitle("Permissions")
        s.permissions.forEach { p ->
            Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.SpaceBetween, verticalAlignment = Alignment.CenterVertically) {
                Text(p.name + if (p.optional) " (optional)" else "", Modifier.weight(1f), style = MaterialTheme.typography.titleSmall)
                if (p.granted) {
                    Text("On", color = MaterialTheme.colorScheme.primary)
                } else if (p.actionable) {
                    TextButton(onClick = { send("open_permission_settings", p.id) }) { Text(p.action ?: "Open settings") }
                }
            }
            if (!p.granted) Text(p.help, style = MaterialTheme.typography.bodySmall)
        }
        s.injectError?.let { Text(it, color = MaterialTheme.colorScheme.error, style = MaterialTheme.typography.bodySmall) }
    }
}

@Composable
private fun BlockedCard(s: UiState) {
    InfoCard(Modifier.fillMaxWidth()) {
        SectionTitle("Blocked machines")
        s.blocked.forEach { b ->
            Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.SpaceBetween, verticalAlignment = Alignment.CenterVertically) {
                Text(b.name)
                TextButton(onClick = { send("unblock", b.nodeId) }) { Text("Unblock") }
            }
        }
    }
}

@Composable
private fun LogCard(s: UiState) {
    InfoCard(Modifier.fillMaxWidth()) {
        SectionTitle("Activity")
        if (s.log.isEmpty()) Text("Nothing yet.", style = MaterialTheme.typography.bodySmall)
        s.log.take(12).forEach { Text(it, style = MaterialTheme.typography.bodySmall) }
    }
}
