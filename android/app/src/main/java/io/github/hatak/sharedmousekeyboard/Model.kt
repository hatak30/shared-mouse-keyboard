package io.github.hatak.sharedmousekeyboard

import org.json.JSONArray
import org.json.JSONObject

data class Rect(val x: Double, val y: Double, val w: Double, val h: Double)

data class Pairing(val kind: String, val code: String?, val error: String?)

data class Peer(
    val nodeId: String,
    val name: String,
    val os: String?,
    val addr: String?,
    val connected: Boolean,
    val trusted: Boolean,
    val fingerprint: String,
    val screen: Rect?,
    val pairing: Pairing?,
)

data class Placement(val nodeId: String, val name: String, val x: Int, val y: Int)

data class Blocked(val nodeId: String, val name: String, val online: Boolean)

data class Permission(
    val id: String,
    val name: String,
    val granted: Boolean,
    val help: String,
    val actionable: Boolean,
    val optional: Boolean,
    val action: String?,
)

data class Transfer(
    val id: String,
    val incoming: Boolean,
    val peer: String,
    val label: String,
    val files: Int,
    val total: Long,
    val done: Long,
    val state: String,
    val error: String?,
    val savedTo: String?,
    val paths: List<String>,
)

data class Me(val nodeId: String, val name: String, val os: String, val port: Int, val fingerprint: String, val screen: Rect)

data class UiState(
    val me: Me,
    val peers: List<Peer>,
    val placements: List<Placement>,
    val settings: JSONObject,
    val manualPeers: List<String>,
    val blocked: List<Blocked>,
    val controlling: String?,
    val controlledBy: String?,
    val permissions: List<Permission>,
    val injectError: String?,
    val log: List<String>,
    val transfers: List<Transfer>,
)

private fun JSONObject.str(key: String): String? = if (isNull(key)) null else getString(key)

private fun <T> JSONArray.map(f: (JSONObject) -> T): List<T> = List(length()) { f(getJSONObject(it)) }

private fun JSONObject.rect(): Rect = Rect(getDouble("x"), getDouble("y"), getDouble("w"), getDouble("h"))

fun parseState(json: String): UiState {
    val o = JSONObject(json)
    val me = o.getJSONObject("me")
    return UiState(
        me = Me(me.getString("node_id"), me.getString("name"), me.getString("os"), me.getInt("port"), me.getString("fingerprint"), me.getJSONObject("screen").rect()),
        peers = o.getJSONArray("peers").map { p ->
            Peer(
                nodeId = p.getString("node_id"),
                name = p.getString("name"),
                os = p.str("os"),
                addr = p.str("addr"),
                connected = p.getBoolean("connected"),
                trusted = p.getBoolean("trusted"),
                fingerprint = p.getString("fingerprint"),
                screen = if (p.isNull("screen")) null else p.getJSONObject("screen").rect(),
                pairing = if (p.isNull("pairing")) null else p.getJSONObject("pairing").let { Pairing(it.getString("kind"), it.str("code"), it.str("error")) },
            )
        },
        placements = o.getJSONObject("layout").getJSONArray("placements").map { Placement(it.getString("node_id"), it.getString("name"), it.getInt("x"), it.getInt("y")) },
        settings = o.getJSONObject("settings"),
        manualPeers = List(o.getJSONArray("manual_peers").length()) { o.getJSONArray("manual_peers").getString(it) },
        blocked = o.getJSONArray("blocked").map { Blocked(it.getString("node_id"), it.getString("name"), it.getBoolean("online")) },
        controlling = o.str("controlling"),
        controlledBy = o.str("controlled_by"),
        permissions = o.getJSONArray("permissions").map {
            Permission(it.getString("id"), it.getString("name"), it.getBoolean("granted"), it.getString("help"), it.getBoolean("actionable"), it.optBoolean("optional"), it.str("action"))
        },
        injectError = o.str("inject_error"),
        log = o.getJSONArray("log").map { it.getString("text") }.reversed(),
        transfers = o.getJSONArray("transfers").map { t ->
            Transfer(
                id = t.getString("id"),
                incoming = t.getBoolean("incoming"),
                peer = t.getString("peer"),
                label = t.getString("label"),
                files = t.getInt("files"),
                total = t.getLong("total"),
                done = t.getLong("done"),
                state = t.getString("state"),
                error = t.str("error"),
                savedTo = t.str("saved_to"),
                paths = List(t.getJSONArray("paths").length()) { t.getJSONArray("paths").getString(it) },
            )
        },
    )
}

fun UiState.withSetting(key: String, value: Any): String = JSONObject(settings.toString()).put(key, value).toString()

fun List<Placement>.toLayoutJson(): String = JSONObject()
    .put("version", 0)
    .put(
        "placements",
        JSONArray(map { JSONObject().put("node_id", it.nodeId).put("name", it.name).put("x", it.x).put("y", it.y) }),
    )
    .toString()
