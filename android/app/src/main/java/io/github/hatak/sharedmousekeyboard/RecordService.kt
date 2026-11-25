package io.github.hatak.sharedmousekeyboard

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.Service
import android.content.Context
import android.content.Intent
import android.content.pm.ServiceInfo
import android.hardware.display.DisplayManager
import android.hardware.display.VirtualDisplay
import android.media.MediaRecorder
import android.media.projection.MediaProjection
import android.media.projection.MediaProjectionManager
import android.os.Build
import android.os.Handler
import android.os.IBinder
import android.os.Looper
import android.util.Log
import androidx.core.app.ServiceCompat
import java.io.File
import java.text.SimpleDateFormat
import java.util.Date
import java.util.Locale
import kotlin.math.max
import kotlin.math.roundToInt

/** Records the screen to an MP4 and offers the file to the paired computers when it ends. */
class RecordService : Service() {
    private var projection: MediaProjection? = null
    private var display: VirtualDisplay? = null
    private var recorder: MediaRecorder? = null
    private var file: File? = null

    override fun onBind(intent: Intent?): IBinder? = null

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        when (intent?.action) {
            START -> start(intent)
            STOP -> finish()
        }
        return START_NOT_STICKY
    }

    private fun start(intent: Intent) {
        if (recorder != null) return
        val nm = getSystemService(NotificationManager::class.java)
        nm.createNotificationChannel(NotificationChannel(CHANNEL, "Screen recording", NotificationManager.IMPORTANCE_LOW))
        val notification = Notification.Builder(this, CHANNEL)
            .setSmallIcon(android.R.drawable.ic_media_play)
            .setContentTitle("Recording the screen")
            .setContentText("Press the recording shortcut on the computer again to stop")
            .setOngoing(true)
            .build()
        val type = if (Build.VERSION.SDK_INT >= 29) ServiceInfo.FOREGROUND_SERVICE_TYPE_MEDIA_PROJECTION else 0
        ServiceCompat.startForeground(this, 2, notification, type)

        try {
            @Suppress("DEPRECATION")
            val data = intent.getParcelableExtra<Intent>(DATA) ?: error("no permission data")
            val manager = getSystemService(Context.MEDIA_PROJECTION_SERVICE) as MediaProjectionManager
            val projection = manager.getMediaProjection(intent.getIntExtra(RESULT, 0), data) ?: error("no projection")
            this.projection = projection
            projection.registerCallback(object : MediaProjection.Callback() {
                override fun onStop() {
                    Handler(Looper.getMainLooper()).post { finish() }
                }
            }, Handler(Looper.getMainLooper()))

            val scale = minOf(1f, 1920f / max(Host.pixelWidth(), Host.pixelHeight()))
            val w = (Host.pixelWidth() * scale).roundToInt() / 2 * 2
            val h = (Host.pixelHeight() * scale).roundToInt() / 2 * 2
            val out = File(Host.downloadDir.apply { mkdirs() }, "Screen recording ${SimpleDateFormat("yyyy-MM-dd HH.mm.ss", Locale.US).format(Date())}.mp4")
            file = out
            val r = if (Build.VERSION.SDK_INT >= 31) MediaRecorder(this) else @Suppress("DEPRECATION") MediaRecorder()
            r.setVideoSource(MediaRecorder.VideoSource.SURFACE)
            r.setOutputFormat(MediaRecorder.OutputFormat.MPEG_4)
            r.setVideoEncoder(MediaRecorder.VideoEncoder.H264)
            r.setVideoSize(w, h)
            r.setVideoFrameRate(30)
            r.setVideoEncodingBitRate(8_000_000)
            r.setOutputFile(out.path)
            r.prepare()
            display = projection.createVirtualDisplay(
                "smk-record", w, h, (Host.density() * 160).toInt(), DisplayManager.VIRTUAL_DISPLAY_FLAG_AUTO_MIRROR, r.surface, null, null,
            )
            r.start()
            recorder = r
            Recorder.recording = true
            Log.i("smk", "recording: started ${w}x$h to ${out.name}")
        } catch (e: Throwable) {
            Log.e("smk", "recording: could not start", e)
            release()
            stopSelf()
        }
    }

    private fun finish() {
        val r = recorder
        val out = file
        if (r == null) {
            release()
            stopSelf()
            return
        }
        val ok = runCatching { r.stop() }.isSuccess
        release()
        Log.i("smk", "recording: stopped ok=$ok ${out?.length()} bytes")
        if (ok && out != null && out.length() > 0) Native.recordingReady(out.path)
        stopSelf()
    }

    private fun release() {
        Recorder.recording = false
        runCatching { display?.release() }
        runCatching { recorder?.release() }
        runCatching { projection?.stop() }
        display = null
        recorder = null
        projection = null
        file = null
    }

    override fun onDestroy() {
        release()
        super.onDestroy()
    }

    companion object {
        const val START = "start"
        const val STOP = "stop"
        const val RESULT = "result"
        const val DATA = "data"
        private const val CHANNEL = "recording"
    }
}
