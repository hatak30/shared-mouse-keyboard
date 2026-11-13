package io.github.hatak.sharedmousekeyboard.ui

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.material3.Button
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.unit.dp
import io.github.hatak.sharedmousekeyboard.Peer
import io.github.hatak.sharedmousekeyboard.UiState

@Composable
fun MachinesTab(s: UiState) {
    LazyColumn(Modifier.fillMaxWidth(), contentPadding = ScreenPadding, verticalArrangement = ItemSpacing) {
        item { ThisDevice(s) }
        items(s.peers, key = { it.nodeId }) { PeerCard(it) }
        if (s.peers.isEmpty()) {
            item {
                Text(
                    "No other machines found yet. Start the app on another computer on the same Wi-Fi, or add it by address below.",
                    style = MaterialTheme.typography.bodyMedium,
                )
            }
        }
        item { AddByAddress(s) }
    }
}

@Composable
private fun ThisDevice(s: UiState) {
    val accessibility = s.permissions.firstOrNull { it.id == "accessibility" }
    InfoCard(Modifier.fillMaxWidth()) {
        SectionTitle(s.me.name)
        Text(
            "${osLabel(s.me.os)} · ${s.me.screen.w.toInt()}×${s.me.screen.h.toInt()} · port ${s.me.port}",
            style = MaterialTheme.typography.bodyMedium,
        )
        s.controlledBy?.let { Text("Controlled by $it", color = MaterialTheme.colorScheme.primary) }
        if (s.controlledBy == null && s.peers.any { it.trusted }) {
            Text(
                "To control this device with a computer's mouse and keyboard, place it next to that computer in the Layout tab (or on the computer), then move the cursor over the shared edge.",
                style = MaterialTheme.typography.bodySmall,
            )
        }
        if (accessibility != null && !accessibility.granted) {
            Text(
                "Other computers cannot control this device until the accessibility service is on.",
                style = MaterialTheme.typography.bodyMedium,
                color = MaterialTheme.colorScheme.error,
            )
            Button(onClick = { send("open_permission_settings", accessibility.id) }) { Text("Turn on") }
        }
    }
}

@Composable
private fun PeerCard(p: Peer) {
    InfoCard(Modifier.fillMaxWidth()) {
        Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.SpaceBetween, verticalAlignment = Alignment.CenterVertically) {
            SectionTitle(p.name)
            Text(
                when {
                    p.trusted && p.connected -> "Connected"
                    p.trusted -> "Offline"
                    else -> "Not paired"
                },
                style = MaterialTheme.typography.labelLarge,
                color = if (p.trusted && p.connected) MaterialTheme.colorScheme.primary else MaterialTheme.colorScheme.onSurfaceVariant,
            )
        }
        Text(
            listOfNotNull(osLabel(p.os), p.addr).joinToString(" · "),
            style = MaterialTheme.typography.bodyMedium,
        )
        val pairing = p.pairing
        when {
            pairing?.kind == "show_code" -> {
                Text("Enter this code on ${p.name}:")
                Text(pairing.code.orEmpty(), style = MaterialTheme.typography.displaySmall)
                TextButton(onClick = { send("cancel_pair", p.nodeId) }) { Text("Cancel") }
            }
            pairing?.kind == "enter_code" -> {
                var code by remember { mutableStateOf("") }
                Text("Type the 6-digit code shown on ${p.name}:")
                OutlinedTextField(
                    value = code,
                    onValueChange = { code = it.filter(Char::isDigit).take(6) },
                    singleLine = true,
                    isError = pairing.error != null,
                    supportingText = pairing.error?.let { { Text(it) } },
                    keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.NumberPassword),
                )
                ButtonRow {
                    Button(enabled = code.length == 6, onClick = { send("submit_code", p.nodeId, code) }) { Text("Pair") }
                    TextButton(onClick = { send("cancel_pair", p.nodeId) }) { Text("Cancel") }
                }
            }
            pairing?.kind == "declined" -> {
                Text("${p.name} does not accept pairing requests.", color = MaterialTheme.colorScheme.error)
                TextButton(onClick = { send("cancel_pair", p.nodeId) }) { Text("OK") }
            }
            p.trusted -> ButtonRow {
                if (p.connected) Button(onClick = { send("control_peer", p.nodeId) }) { Text("Control") }
                if (p.connected) OutlinedButton(onClick = { send("send_snapshot", p.nodeId) }) { Text("Send screenshot") }
                OutlinedButton(onClick = { send("unpair", p.nodeId) }) { Text("Unpair") }
            }
            else -> ButtonRow {
                Button(onClick = { send("pair", p.nodeId) }) { Text("Pair") }
                TextButton(onClick = { send("block", p.nodeId) }) { Text("Block") }
            }
        }
    }
}

@Composable
private fun AddByAddress(s: UiState) {
    var address by remember { mutableStateOf("") }
    InfoCard(Modifier.fillMaxWidth()) {
        SectionTitle("Add by address")
        Text(
            "Use this when the network blocks discovery. Example: 192.168.1.20",
            style = MaterialTheme.typography.bodyMedium,
        )
        OutlinedTextField(
            value = address,
            onValueChange = { address = it.trim() },
            singleLine = true,
            label = { Text("IP address or host name") },
            modifier = Modifier.fillMaxWidth(),
        )
        Button(enabled = address.isNotEmpty(), onClick = {
            send("connect", address)
            address = ""
        }) { Text("Connect") }
        s.manualPeers.forEach { addr ->
            Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.SpaceBetween, verticalAlignment = Alignment.CenterVertically) {
                Text(addr)
                TextButton(onClick = { send("remove_manual_peer", addr) }) { Text("Remove") }
            }
        }
    }
}
