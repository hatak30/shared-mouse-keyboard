package io.github.hatak.sharedmousekeyboard.ui

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.padding
import androidx.compose.material3.Card
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.unit.dp
import io.github.hatak.sharedmousekeyboard.Store

val ScreenPadding = PaddingValues(16.dp)
val ItemSpacing = Arrangement.spacedBy(12.dp)

fun osLabel(os: String?) = when (os) {
    "MacOs" -> "macOS"
    "Windows" -> "Windows"
    "Linux" -> "Linux"
    "Android" -> "Android"
    else -> "Unknown system"
}

fun send(name: String, a: String = "", b: String = "") = Store.command(name, a, b)

@Composable
fun SectionTitle(text: String, modifier: Modifier = Modifier) {
    Text(text, style = MaterialTheme.typography.titleMedium, modifier = modifier)
}

@Composable
fun InfoCard(modifier: Modifier = Modifier, content: @Composable () -> Unit) {
    Card(modifier) {
        Column(Modifier.padding(16.dp), verticalArrangement = Arrangement.spacedBy(8.dp)) { content() }
    }
}

@Composable
fun ButtonRow(content: @Composable () -> Unit) {
    Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) { content() }
}
