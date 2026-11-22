package com.downbest.media

import android.app.Activity
import app.tauri.annotation.Command
import app.tauri.annotation.InvokeArg
import app.tauri.annotation.TauriPlugin
import app.tauri.plugin.Channel
import app.tauri.plugin.Invoke
import app.tauri.plugin.JSObject
import app.tauri.plugin.Plugin
import dev.ffmpegkit_maintained.ytdlp.DownloadProgressCallback
import dev.ffmpegkit_maintained.ytdlp.YtDlp
import dev.ffmpegkit_maintained.ytdlp.YtDlpRequest
import com.arthenica.ffmpegkit.FFmpegKit
import com.arthenica.ffmpegkit.ReturnCode
import java.io.File
import java.util.concurrent.TimeUnit

@InvokeArg
class DownloadArgs {
    var url: String = ""
    var destination: String = ""
    var kind: String = "auto"
    var concurrency: Int = 8
    var onProgress: Channel? = null
}

@TauriPlugin
class MediaPlugin(private val activity: Activity) : Plugin(activity) {
    @Command
    fun download(invoke: Invoke) {
        val args = invoke.parseArgs(DownloadArgs::class.java)
        Thread({
            try {
                require(args.url.startsWith("https://") || args.url.startsWith("http://")) {
                    "Only HTTP and HTTPS media URLs are supported."
                }
                val folder = File(args.destination)
                if (!folder.exists() && !folder.mkdirs()) {
                    throw IllegalStateException("Could not create the selected save folder.")
                }
                if (!folder.isDirectory) {
                    throw IllegalStateException("The selected save location is not a folder.")
                }

                YtDlp.init(activity.applicationContext)
                val startedAt = System.currentTimeMillis()
                val template = File(folder, "%(title).180B [%(id)s].%(ext)s").absolutePath
                val request = YtDlpRequest(args.url)
                    .setOutputTemplate(template)
                    .addOption("--no-playlist")
                    .addOption("--newline")

                when (args.kind) {
                    "audio" -> request.addOption("-f", "bestaudio/best")
                    "video-only" -> request.addOption("-f", "bestvideo")
                    "video" -> request.addOption("-f", "best[ext=mp4]/best")
                    else -> request.addOption("-f", "best[ext=mp4]/best")
                }

                val future = YtDlp.executeAsync(request, DownloadProgressCallback { progress, eta, line ->
                    val payload = JSObject()
                    payload.put("progress", progress.toDouble())
                    payload.put("etaSeconds", eta)
                    payload.put("line", line ?: "")
                    try {
                        args.onProgress?.send(payload)
                    } catch (_: Exception) {
                        // A closed progress channel must not abort the transfer.
                    }
                })
                val response = future.get(6, TimeUnit.HOURS)
                if (!response.isSuccess) {
                    throw IllegalStateException("yt-dlp failed with exit code ${response.exitCode}. Check whether the source requires sign-in, has changed, or blocks this downloader.")
                }

                val downloaded = folder.listFiles()
                    ?.filter { it.isFile && it.lastModified() >= startedAt && !it.name.endsWith(".part") }
                    ?.maxByOrNull { it.lastModified() }
                    ?: throw IllegalStateException("The extractor finished but no output file was found.")

                var output = downloaded
                // Android has no desktop ffmpeg executable. The bundled LGPL
                // FFmpegKit converts audio-only downloads into a portable M4A.
                if (args.kind == "audio") {
                    val m4a = File(downloaded.parentFile, downloaded.nameWithoutExtension + ".m4a")
                    val command = "-y -i ${shellQuote(downloaded.absolutePath)} -vn -c:a aac -b:a 192k -threads 2 ${shellQuote(m4a.absolutePath)}"
                    val session = FFmpegKit.execute(command)
                    if (!ReturnCode.isSuccess(session.returnCode)) {
                        throw IllegalStateException("Audio was downloaded, but the built-in audio converter failed. Original file: ${downloaded.name}")
                    }
                    downloaded.delete()
                    output = m4a
                }

                val result = JSObject()
                result.put("filePath", output.absolutePath)
                result.put("fileName", output.name)
                result.put("format", args.kind)
                invoke.resolve(result)
            } catch (error: Exception) {
                invoke.reject(error.message ?: "Mobile media download failed.")
            }
        }, "DownBest-media-download").start()
    }

    private fun shellQuote(value: String): String =
        "'" + value.replace("'", "'\\''") + "'"
}
