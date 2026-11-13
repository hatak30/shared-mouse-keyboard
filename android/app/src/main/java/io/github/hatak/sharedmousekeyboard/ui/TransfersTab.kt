package io.github.hatak.sharedmousekeyboard.ui

import android.text.format.Formatter
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.material3.Button
import androidx.compose.material3.LinearProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.unit.dp
import io.github.hatak.sharedmousekeyboard.Host
import io.github.hatak.sharedmousekeyboard.Transfer
import io.github.hatak.sharedmousekeyboard.UiState

@Composable
fun TransfersTab(s: UiState) {
    LazyColumn(Modifier.fillMaxSize(), contentPadding = ScreenPadding, verticalArrangement = ItemSpacing) {
        item {
            Text(
                "Copy files on a computer and they show up here. Nothing is sent until you choose Save.",
                style = MaterialTheme.typography.bodyMedium,
            )
        }
        if (s.transfers.isEmpty()) item { Text("No copied files yet.") }
        items(s.transfers, key = { it.id }) { TransferCard(it) }
    }
}

@Composable
private fun TransferCard(t: Transfer) {
    val context = LocalContext.current
    InfoCard(Modifier.fillMaxWidth()) {
        SectionTitle(t.label)
        Text(
            "${if (t.incoming) "From" else "To"} ${t.peer} · ${t.files} ${if (t.files == 1) "file" else "files"} · ${Formatter.formatShortFileSize(context, t.total)}",
            style = MaterialTheme.typography.bodyMedium,
        )
        when (t.state) {
            "pending" -> Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                Button(onClick = { send("save_transfer_to", t.id, Host.downloadDir.path) }) { Text("Save") }
                OutlinedButton(onClick = { send("dismiss_transfer", t.id) }) { Text("Ignore") }
            }
            "running" -> {
                LinearProgressIndicator(progress = { if (t.total > 0) t.done.toFloat() / t.total else 0f }, modifier = Modifier.fillMaxWidth())
                Text("${Formatter.formatShortFileSize(context, t.done)} of ${Formatter.formatShortFileSize(context, t.total)}")
                OutlinedButton(onClick = { send("cancel_transfer", t.id) }) { Text("Cancel") }
            }
            "done" -> {
                Text("Saved to ${t.savedTo ?: Host.downloadDir.path}", style = MaterialTheme.typography.bodyMedium)
                OutlinedButton(onClick = { send("dismiss_transfer", t.id) }) { Text("Dismiss") }
            }
            else -> {
                Text(t.error ?: if (t.state == "cancelled") "Cancelled" else "Failed", color = MaterialTheme.colorScheme.error)
                OutlinedButton(onClick = { send("dismiss_transfer", t.id) }) { Text("Dismiss") }
            }
        }
    }
}
