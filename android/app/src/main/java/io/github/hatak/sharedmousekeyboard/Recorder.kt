package io.github.hatak.sharedmousekeyboard

import android.app.Activity
import android.content.Context
import android.content.Intent
import android.media.projection.MediaProjectionManager
import android.os.Bundle
import android.util.Log

/** Starts and stops screen recording on the engine's behalf (see [Host.toggleRecording]). */
object Recorder {
    @Volatile var recording = false

    fun toggle(): Boolean {
        val service = InputService.instance ?: return false
        val app = Host.app
        if (recording) {
            app.startService(Intent(app, RecordService::class.java).setAction(RecordService.STOP))
            return true
        }
        service.startActivity(Intent(app, RecordActivity::class.java).addFlags(Intent.FLAG_ACTIVITY_NEW_TASK or Intent.FLAG_ACTIVITY_NO_ANIMATION))
        service.acceptScreenCapturePrompt()
        return true
    }
}

/** Invisible activity that shows Android's "start recording" prompt and hands the result to [RecordService]. */
class RecordActivity : Activity() {
    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        val manager = getSystemService(Context.MEDIA_PROJECTION_SERVICE) as MediaProjectionManager
        @Suppress("DEPRECATION")
        startActivityForResult(manager.createScreenCaptureIntent(), 1)
    }

    @Deprecated("Deprecated in Java")
    override fun onActivityResult(requestCode: Int, resultCode: Int, data: Intent?) {
        if (resultCode == RESULT_OK && data != null) {
            startForegroundService(
                Intent(this, RecordService::class.java).setAction(RecordService.START)
                    .putExtra(RecordService.RESULT, resultCode).putExtra(RecordService.DATA, data),
            )
        } else {
            Log.w("smk", "recording: permission to record was refused")
        }
        finish()
    }
}
