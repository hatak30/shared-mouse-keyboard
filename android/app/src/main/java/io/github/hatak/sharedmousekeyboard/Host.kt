package io.github.hatak.sharedmousekeyboard

import android.Manifest
import android.app.Application
import android.content.ClipData
import android.content.ClipboardManager
import android.content.Context
import android.content.Intent
import android.content.pm.PackageManager
import android.hardware.display.DisplayManager
import android.os.Build
import android.os.Environment
import android.os.PowerManager
import android.provider.Settings
import android.util.DisplayMetrics
import android.util.Log
import android.view.Display
import java.io.File

/** Called from the Rust engine (see `crates/smk-android`). Method names and signatures are part of the JNI contract. */
object Host {
    lateinit var app: Application
    val downloadDir: File
        get() = app.getExternalFilesDir(Environment.DIRECTORY_DOWNLOADS) ?: File(app.filesDir, "downloads")

    @Volatile private var started = false

    @Synchronized
    fun startEngine() {
        if (started) return
        val config = File(app.filesDir, "config").apply { mkdirs() }
        val downloads = downloadDir.apply { mkdirs() }
        started = Native.start(this, config.path, downloads.path)
        if (!started) {
            Log.e("smk", "engine failed to start")
            return
        }
        Store.refresh()
        val name = Store.state.value?.me?.name
        if (name == null || name == "localhost" || name.isBlank()) Store.command("set_name", Build.MODEL)
    }

    fun clipboardChangedLocally() {
        if (started) Native.clipboardChanged()
    }

    // ---- called by the engine

    fun inject(kind: Int, x: Double, y: Double, a: Int, b: Int): Boolean =
        InputService.instance?.inject(kind, x, y, a, b) ?: false

    fun screenWidth(): Int = realMetrics().widthPixels

    fun screenHeight(): Int = realMetrics().heightPixels

    fun clipboardText(): String? {
        val clip = app.getSystemService(ClipboardManager::class.java).primaryClip ?: return null
        if (clip.itemCount == 0) return null
        return clip.getItemAt(0).coerceToText(app)?.toString()
    }

    fun setClipboardText(text: String) {
        app.getSystemService(ClipboardManager::class.java).setPrimaryClip(ClipData.newPlainText("smk", text))
    }

    fun permissionGranted(id: String): Boolean = when (id) {
        "accessibility" -> InputService.instance != null
        "notifications" -> Build.VERSION.SDK_INT < 33 ||
            app.checkSelfPermission(Manifest.permission.POST_NOTIFICATIONS) == PackageManager.PERMISSION_GRANTED
        "battery" -> app.getSystemService(PowerManager::class.java).isIgnoringBatteryOptimizations(app.packageName)
        else -> false
    }

    fun openSettings(id: String) {
        val intent = when (id) {
            "notifications" -> Intent(Settings.ACTION_APP_NOTIFICATION_SETTINGS).putExtra(Settings.EXTRA_APP_PACKAGE, app.packageName)
            "battery" -> Intent(Settings.ACTION_IGNORE_BATTERY_OPTIMIZATION_SETTINGS)
            else -> Intent(Settings.ACTION_ACCESSIBILITY_SETTINGS)
        }
        app.startActivity(intent.addFlags(Intent.FLAG_ACTIVITY_NEW_TASK))
    }

    fun stateChanged() = Store.refresh()

    @Suppress("DEPRECATION")
    private fun realMetrics(): DisplayMetrics {
        val display = app.getSystemService(DisplayManager::class.java).getDisplay(Display.DEFAULT_DISPLAY)
        return DisplayMetrics().also { display.getRealMetrics(it) }
    }
}
