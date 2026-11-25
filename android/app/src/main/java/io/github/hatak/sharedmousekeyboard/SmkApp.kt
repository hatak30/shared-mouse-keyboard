package io.github.hatak.sharedmousekeyboard

import android.app.Application
import android.content.ClipboardManager
import kotlin.concurrent.thread

class SmkApp : Application() {
    override fun onCreate() {
        super.onCreate()
        Host.app = this
        getSystemService(ClipboardManager::class.java).addPrimaryClipChangedListener { Host.clipboardChangedLocally() }
        thread(name = "smk-start") { Host.startEngine() }
    }
}
