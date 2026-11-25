package io.github.hatak.sharedmousekeyboard.ui

import androidx.compose.foundation.BorderStroke
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.Button
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.foundation.clickable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import io.github.hatak.sharedmousekeyboard.Placement
import io.github.hatak.sharedmousekeyboard.UiState
import io.github.hatak.sharedmousekeyboard.toLayoutJson

@Composable
fun LayoutTab(s: UiState) {
    var draft by remember(s.placements) { mutableStateOf(s.placements) }
    var selected by remember { mutableStateOf<String?>(null) }

    val minX = (draft.minOfOrNull { it.x } ?: 0) - 1
    val maxX = (draft.maxOfOrNull { it.x } ?: 0) + 1
    val minY = (draft.minOfOrNull { it.y } ?: 0) - 1
    val maxY = (draft.maxOfOrNull { it.y } ?: 0) + 1

    Column(Modifier.fillMaxSize().padding(16.dp), verticalArrangement = Arrangement.spacedBy(12.dp)) {
        SectionTitle("Screen layout")
        Text(
            "Arrange the machines like they stand on your desk. Tap a machine, then tap an empty square to move it. The cursor crosses the shared edge between neighbours.",
            style = MaterialTheme.typography.bodyMedium,
        )
        Column(
            Modifier.weight(1f).verticalScroll(rememberScrollState()).horizontalScroll(rememberScrollState()),
            verticalArrangement = Arrangement.spacedBy(8.dp),
        ) {
            for (y in minY..maxY) {
                Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                    for (x in minX..maxX) {
                        val here = draft.firstOrNull { it.x == x && it.y == y }
                        val isMe = here?.nodeId == s.me.nodeId
                        Cell(
                            placement = here,
                            isMe = isMe,
                            isController = here != null &&
                                ((isMe && s.controlling != null) || (!isMe && s.controlledBy == here.name)),
                            isTarget = here != null && !isMe && s.controlling == here.name,
                            isSelected = here != null && here.nodeId == selected,
                            onClick = {
                                if (here != null) {
                                    selected = if (selected == here.nodeId) null else here.nodeId
                                } else selected?.let { id ->
                                    draft = draft.map { if (it.nodeId == id) it.copy(x = x, y = y) else it }
                                    selected = null
                                }
                            },
                        )
                    }
                }
            }
        }
        Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
            Button(enabled = draft != s.placements, onClick = { send("set_layout", draft.toLayoutJson()) }) { Text("Save layout") }
            OutlinedButton(enabled = draft != s.placements, onClick = {
                draft = s.placements
                selected = null
            }) { Text("Reset") }
        }
    }
}

@Composable
private fun Cell(
    placement: Placement?,
    isMe: Boolean,
    isController: Boolean,
    isTarget: Boolean,
    isSelected: Boolean,
    onClick: () -> Unit,
) {
    val colors = MaterialTheme.colorScheme
    Surface(
        modifier = Modifier.size(width = 104.dp, height = 72.dp).clickable(onClick = onClick),
        shape = MaterialTheme.shapes.medium,
        color = when {
            placement == null -> colors.surface
            isSelected -> colors.primaryContainer
            isController -> colors.tertiaryContainer
            isMe -> colors.secondaryContainer
            else -> colors.surfaceVariant
        },
        border = BorderStroke(
            if (isSelected || isController || isTarget) 3.dp else 1.dp,
            when {
                isController -> colors.tertiary
                isSelected || isTarget -> colors.primary
                else -> colors.outlineVariant
            },
        ),
    ) {
        if (placement != null) {
            Column(Modifier.padding(8.dp), verticalArrangement = Arrangement.Center, horizontalAlignment = Alignment.CenterHorizontally) {
                Text(placement.name, maxLines = 2, overflow = TextOverflow.Ellipsis, textAlign = TextAlign.Center, style = MaterialTheme.typography.labelLarge)
                when {
                    isController -> Text(if (isMe) "controlling now" else "controlling", style = MaterialTheme.typography.labelSmall)
                    isTarget -> Text("being controlled", style = MaterialTheme.typography.labelSmall)
                    isMe -> Text("this device", style = MaterialTheme.typography.labelSmall)
                }
            }
        }
    }
}
