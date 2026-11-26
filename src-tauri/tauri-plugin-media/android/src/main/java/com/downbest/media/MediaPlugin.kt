package com.downbest.media

import android.app.Activity
import android.content.ContentValues
import android.content.Intent
import android.net.Uri
import android.os.Build
import android.os.Environment
import android.provider.MediaStore
import android.webkit.MimeTypeMap
import androidx.core.content.FileProvider
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
import java.util.concurrent.ConcurrentHashMap
import java.util.concurrent.Future
import java.util.concurrent.TimeUnit

@InvokeArg
class DownloadArgs {
    var url: String = ""
    var destination: String = ""
    var jobId: String = ""
    var kind: String = "auto"
    var concurrency: Int = 8
    var cookiesFile: String? = null
    var formatSelector: String? = null
    var onProgress: Channel? = null
}

@InvokeArg
class CancelDownloadArgs {
    var jobId: String = ""
}

@InvokeArg
class PublishDownloadArgs {
    var filePath: String = ""
}

@InvokeArg
class OpenDownloadArgs {
    var contentUri: String = ""
}

@InvokeArg
class InspectArgs {
    var url: String = ""
    var flatPlaylist: Boolean = true
}

@TauriPlugin
class MediaPlugin(private val activity: Activity) : Plugin(activity) {
    companion object {
        private val ytdlpInitLock = Any()
        @Volatile private var ytdlpInitialized = false
    }

    private val activeDownloads = ConcurrentHashMap<String, Future<*>>()
    private val jobGenerations = ConcurrentHashMap<String, Long>()
    private val cancelledGenerations = ConcurrentHashMap<String, Long>()

    private fun isCancelled(jobId: String, generation: Long): Boolean =
        jobGenerations[jobId] != generation || cancelledGenerations[jobId] == generation

    @Command
    fun cancelDownload(invoke: Invoke) {
        val args = invoke.parseArgs(CancelDownloadArgs::class.java)
        jobGenerations[args.jobId]?.let { generation ->
            cancelledGenerations[args.jobId] = generation
        }
        activeDownloads[args.jobId]?.cancel(true)
        val result = JSObject()
        result.put("cancelled", true)
        invoke.resolve(result)
    }

    private fun copyToAppDownloads(source: File): File {
        val folder = activity.getExternalFilesDir(Environment.DIRECTORY_DOWNLOADS)
            ?: File(activity.filesDir, "Downloads")
        if (!folder.exists() && !folder.mkdirs()) {
            throw IllegalStateException("Could not create the app Downloads folder.")
        }
        require(folder.isDirectory) { "The app Downloads location is not a folder." }
        val target = File(folder, source.name)
        if (source.absoluteFile != target.absoluteFile) {
            source.inputStream().buffered().use { input ->
                target.outputStream().buffered().use { output -> input.copyTo(output, 256 * 1024) }
            }
        }
        require(target.isFile && target.length() > 0L) {
            "The downloaded file could not be copied to the app Downloads folder."
        }
        return target
    }

    @Command
    fun publishDownload(invoke: Invoke) {
        val args = invoke.parseArgs(PublishDownloadArgs::class.java)
        Thread({
            var publishedUri: Uri? = null
            try {
                val source = File(args.filePath)
                require(source.isFile && source.canRead()) {
                    "The finished file is missing from temporary storage."
                }
                if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q) {
                    val mimeType = MimeTypeMap.getSingleton()
                        .getMimeTypeFromExtension(source.extension.lowercase())
                        ?: "application/octet-stream"
                    val values = ContentValues().apply {
                        put(MediaStore.MediaColumns.DISPLAY_NAME, source.name)
                        put(MediaStore.MediaColumns.MIME_TYPE, mimeType)
                        put(MediaStore.MediaColumns.RELATIVE_PATH, Environment.DIRECTORY_DOWNLOADS)
                        put(MediaStore.MediaColumns.IS_PENDING, 1)
                    }
                    val resolver = activity.contentResolver
                    publishedUri = resolver.insert(MediaStore.Downloads.EXTERNAL_CONTENT_URI, values)
                        ?: throw IllegalStateException("Android could not create a file in the Downloads folder.")
                    val destination = resolver.openOutputStream(publishedUri!!, "w")
                        ?: throw IllegalStateException("Android could not open the new Downloads file.")
                    source.inputStream().buffered().use { input ->
                        destination.buffered().use { output -> input.copyTo(output, 256 * 1024) }
                    }
                    val ready = ContentValues().apply {
                        put(MediaStore.MediaColumns.IS_PENDING, 0)
                    }
                    resolver.update(publishedUri!!, ready, null, null)
                    val result = JSObject()
                    result.put("contentUri", publishedUri.toString())
                    result.put("fileName", source.name)
                    source.delete()
                    invoke.resolve(result)
                } else {
                    // Android 7–9 cannot use MediaStore.Downloads/RELATIVE_PATH.
                    val fallback = copyToAppDownloads(source)
                    val result = JSObject()
                    result.put("contentUri", fallback.absolutePath)
                    result.put("fileName", fallback.name)
                    source.delete()
                    invoke.resolve(result)
                }
            } catch (error: Exception) {
                publishedUri?.let { activity.contentResolver.delete(it, null, null) }
                try {
                    // Keep a successful transfer Completed if the shared Downloads
                    // provider rejects the final copy on a particular Android build.
                    val source = File(args.filePath)
                    require(source.isFile && source.canRead()) {
                        "The finished file is missing from temporary storage."
                    }
                    val fallback = copyToAppDownloads(source)
                    val result = JSObject()
                    result.put("contentUri", fallback.absolutePath)
                    result.put("fileName", fallback.name)
                    source.delete()
                    invoke.resolve(result)
                } catch (fallbackError: Exception) {
                    invoke.reject(
                        "Downloads save failed: ${error.message ?: "unknown storage error"}. " +
                            "App storage fallback failed: ${fallbackError.message ?: "unknown error"}."
                    )
                }
            }
        }, "DownBest-publish-download").start()
    }

    @Command
    fun openDownload(invoke: Invoke) {
        val args = invoke.parseArgs(OpenDownloadArgs::class.java)
        activity.runOnUiThread {
            try {
                val raw = args.contentUri
                val uri = if (raw.startsWith("content://")) {
                    Uri.parse(raw)
                } else {
                    val file = File(raw)
                    require(file.isFile && file.canRead()) { "The downloaded file is no longer available." }
                    FileProvider.getUriForFile(
                        activity,
                        "${activity.packageName}.downbest.media.fileprovider",
                        file
                    )
                }
                val mimeType = activity.contentResolver.getType(uri)
                    ?: MimeTypeMap.getSingleton().getMimeTypeFromExtension(
                        MimeTypeMap.getFileExtensionFromUrl(uri.toString()).lowercase()
                    )
                    ?: "application/octet-stream"
                val view = Intent(Intent.ACTION_VIEW).setDataAndType(uri, mimeType)
                    .addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION)
                activity.startActivity(Intent.createChooser(view, "Open downloaded file"))
                val result = JSObject()
                result.put("opened", true)
                invoke.resolve(result)
            } catch (error: Exception) {
                invoke.reject(error.message ?: "No installed app can open this file type.")
            }
        }
    }

    private fun ensureYtDlpInitialized() {
        if (ytdlpInitialized) return
        synchronized(ytdlpInitLock) {
            if (!ytdlpInitialized) {
                YtDlp.init(activity.applicationContext)
                ytdlpInitialized = true
            }
        }
    }

    private fun javascriptRuntimePath(): String? {
        val abi = Build.SUPPORTED_ABIS.firstOrNull { it == "arm64-v8a" || it == "x86_64" }
            ?: return null
        val runtime = File(activity.applicationInfo.nativeLibraryDir, "libqjs.so")
        if (runtime.isFile && !runtime.canExecute()) runtime.setExecutable(true, false)
        return runtime.absolutePath.takeIf { runtime.isFile && runtime.canExecute() }
    }

    private fun addJavascriptRuntime(request: YtDlpRequest): Boolean {
        val runtime = javascriptRuntimePath() ?: return false
        request.addOption("--js-runtimes", "quickjs:$runtime")
        request.addOption("--remote-components", "ejs:github")
        return true
    }

    @Command
    fun inspect(invoke: Invoke) {
        val args = invoke.parseArgs(InspectArgs::class.java)
        Thread({
            try {
                require(args.url.startsWith("https://") || args.url.startsWith("http://")) {
                    "Only HTTP and HTTPS media links are supported."
                }
                ensureYtDlpInitialized()
                val request = YtDlpRequest(args.url)
                    .addOption("--dump-single-json")
                    .addOption("--skip-download")
                    .addOption("--no-warnings")
                    .addOption("--no-progress")
                    .addOption("--no-colors")
                    .addOption("--no-call-home")
                    .addOption("--socket-timeout", "20")
                    .addOption("--retries", "2")
                if (args.flatPlaylist) request.addOption("--flat-playlist")
                else request.addOption("--no-playlist")
                val runtimeAvailable = addJavascriptRuntime(request)
                val response = YtDlp.executeAsync(request, DownloadProgressCallback { _, _, _ -> }).get(180, TimeUnit.SECONDS)
                val result = JSObject()
                result.put("output", response.output ?: "")
                result.put("errorOutput", response.errorOutput ?: "")
                result.put("exitCode", response.exitCode)
                if (!response.isSuccess && !runtimeAvailable) {
                    result.put("errorOutput",
                        (response.errorOutput ?: "") + "\nDownBest QuickJS runtime was not found in this APK.")
                }
                invoke.resolve(result)
            } catch (error: Exception) {
                val message = error.cause?.message ?: error.message ?: "Could not inspect this media link."
                invoke.reject(message)
            }
        }, "DownBest-media-inspect").start()
    }

    @Command
    fun download(invoke: Invoke) {
        val args = invoke.parseArgs(DownloadArgs::class.java)
        val generation = jobGenerations.merge(args.jobId, 1L) { previous, _ -> previous + 1L } ?: 1L
        Thread({
            try {
                require(args.url.startsWith("https://") || args.url.startsWith("http://")) {
                    "Only HTTP and HTTPS media URLs are supported."
                }
                val destinationFolder = File(args.destination)
                if (!destinationFolder.exists() && !destinationFolder.mkdirs()) {
                    throw IllegalStateException("Could not create the selected save folder.")
                }
                if (!destinationFolder.isDirectory) {
                    throw IllegalStateException("The selected save location is not a folder.")
                }

                require(args.jobId.matches(Regex("[A-Za-z0-9_-]{8,64}"))) {
                    "The download job identifier is invalid."
                }
                // Keep yt-dlp's split audio/video tracks, .part files and fragment
                // chunks in a per-job workspace, never beside finished Downloads.
                val folder = File(destinationFolder, ".downbest-work-${args.jobId}")
                if (!folder.exists() && !folder.mkdirs()) {
                    throw IllegalStateException("Could not create a private media workspace.")
                }
                if (!folder.isDirectory) {
                    throw IllegalStateException("The private media workspace is not a folder.")
                }
                if (isCancelled(args.jobId, generation)) throw InterruptedException("Download cancelled.")
                ensureYtDlpInitialized()
                if (isCancelled(args.jobId, generation)) throw InterruptedException("Download cancelled.")
                val template = File(folder, "%(title).180B [%(id)s] - ${args.jobId}.%(ext)s").absolutePath
                val request = YtDlpRequest(args.url)
                    .setOutputTemplate(template)
                    .addOption("--no-playlist")
                    .addOption("--newline")
                    .addOption("--continue")
                    .addOption("--part")
                    .addOption("--socket-timeout", "20")
                    .addOption("--retries", "4")
                    .addOption("--fragment-retries", "4")
                    .addOption("--extractor-retries", "2")
                    .addOption("--file-access-retries", "2")
                    .addOption("--concurrent-fragments", args.concurrency.coerceIn(1, 16).toString())
                    .addOption("--buffer-size", "1M")
                    .addOption("--print", "after_move:__DOWNBEST_FILE__%(filepath)s")

                addJavascriptRuntime(request)
                if (!args.cookiesFile.isNullOrBlank()) {
                    val cookieFile = File(args.cookiesFile!!)
                    require(cookieFile.isFile && cookieFile.canRead()) {
                        "The imported cookies.txt file is no longer accessible. Import it again and retry."
                    }
                    request.addOption("--cookies", cookieFile.absolutePath)
                }

                val selector = args.formatSelector
                val exactFormat = selector?.takeIf { it.startsWith("format-") }
                    ?.removePrefix("format-")
                    ?.takeIf { it.isNotEmpty() && it.matches(Regex("[A-Za-z0-9._-]+")) }
                val maxHeight = selector?.takeIf { it.startsWith("max-height-") }
                    ?.removePrefix("max-height-")?.takeIf { it.isNotEmpty() && it.all(Char::isDigit) }
                val selectedFormat = when {
                    args.kind == "audio" -> "bestaudio/best"
                    exactFormat != null -> exactFormat
                    args.kind == "video-only" -> if (maxHeight != null) "bestvideo[height<=$maxHeight]/bestvideo" else "bestvideo"
                    maxHeight != null -> "best[height<=$maxHeight][ext=mp4][acodec!=none]/best[height<=$maxHeight][acodec!=none]"
                    else -> "best[ext=mp4][acodec!=none]/best[acodec!=none]"
                }
                request.addOption("-f", selectedFormat)

                if (isCancelled(args.jobId, generation)) throw InterruptedException("Download cancelled.")
                val progressSpeedLock = Any()
                var lastProgressBytes = 0L
                var lastProgressAtNanos = System.nanoTime()
                var smoothedSpeedBytesPerSecond = 0L
                val future = YtDlp.executeAsync(request, DownloadProgressCallback { progress, eta, line ->
                    val progressLine = line ?: ""
                    val payload = JSObject()
                    payload.put("progress", progress.toDouble())
                    payload.put("etaSeconds", eta)
                    payload.put("line", progressLine)
                    val jobMarker = " - ${args.jobId}."
                    val matchingFiles = folder.listFiles()
                        ?.filter { it.isFile && it.name.contains(jobMarker) }
                        ?: emptyList()
                    val parsedTotal = parseTotalBytes(progressLine)
                    val partialFiles = matchingFiles.filter {
                        it.name.endsWith(".part") || it.name.contains(".part-Frag")
                    }
                    val diskBytes = if (partialFiles.isNotEmpty()) partialFiles.sumOf { it.length() }
                        else matchingFiles.filterNot { it.name.endsWith(".ytdl") }
                            .maxOfOrNull { it.length() } ?: 0L
                    val downloadedBytes = if (parsedTotal != null && progress > 0.0) {
                        (parsedTotal * progress / 100.0).toLong().coerceAtLeast(0L)
                    } else diskBytes
                    payload.put("downloadedBytes", downloadedBytes)
                    val nowNanos = System.nanoTime()
                    val sampledSpeed = synchronized(progressSpeedLock) {
                        val elapsedNanos = nowNanos - lastProgressAtNanos
                        if (elapsedNanos >= 150_000_000L && downloadedBytes > lastProgressBytes) {
                            val elapsedSeconds = elapsedNanos / 1_000_000_000.0
                            val instantSpeed = ((downloadedBytes - lastProgressBytes) / elapsedSeconds).toLong()
                            smoothedSpeedBytesPerSecond = if (smoothedSpeedBytesPerSecond == 0L) {
                                instantSpeed
                            } else {
                                (smoothedSpeedBytesPerSecond * 0.35 + instantSpeed * 0.65).toLong()
                            }
                            lastProgressBytes = downloadedBytes
                            lastProgressAtNanos = nowNanos
                        } else if (elapsedNanos >= 2_000_000_000L) {
                            smoothedSpeedBytesPerSecond = 0L
                            lastProgressBytes = downloadedBytes
                            lastProgressAtNanos = nowNanos
                        }
                        smoothedSpeedBytesPerSecond
                    }
                    val speedBytesPerSecond = parseSpeedBytesPerSecond(progressLine) ?: sampledSpeed
                    payload.put("speedBytesSec", speedBytesPerSecond)
                    val estimatedTotal = parsedTotal ?: if (progress > 0.0 && downloadedBytes > 0L) {
                        (downloadedBytes * 100.0 / progress).toLong()
                    } else null
                    if (estimatedTotal != null && estimatedTotal > 0L) {
                        payload.put("totalBytes", estimatedTotal)
                    }
                    try {
                        args.onProgress?.send(payload)
                    } catch (_: Exception) {
                        // A closed progress channel must not abort the transfer.
                    }
                })
                val alreadyRunning = activeDownloads.putIfAbsent(args.jobId, future)
                if (alreadyRunning != null) {
                    future.cancel(true)
                    throw IllegalStateException("The previous transfer is still stopping. Please retry Resume.")
                }
                if (isCancelled(args.jobId, generation)) {
                    activeDownloads.remove(args.jobId, future)
                    future.cancel(true)
                    throw InterruptedException("Download cancelled.")
                }
                val response = try {
                    future.get(6, TimeUnit.HOURS)
                } finally {
                    activeDownloads.remove(args.jobId, future)
                }
                if (isCancelled(args.jobId, generation)) throw InterruptedException("Download cancelled.")
                if (!response.isSuccess) {
                    throw IllegalStateException("yt-dlp failed with exit code ${response.exitCode}. Check whether the source requires sign-in, has changed, or blocks this downloader.")
                }

                val jobMarker = " - ${args.jobId}."
                val printedPath = Regex("(?m)^__DOWNBEST_FILE__(.+)$")
                    .find(response.output ?: "")?.groupValues?.get(1)?.trim()
                val printedFile = printedPath?.takeIf { it.isNotEmpty() }?.let(::File)?.takeIf { it.isFile }
                val downloaded = printedFile ?: folder.listFiles()
                    ?.filter { it.isFile && it.name.contains(jobMarker) && !isExtractorTemporaryFile(it.name) }
                    ?.maxByOrNull { it.lastModified() }
                    ?: throw IllegalStateException("The extractor finished but no final output file was found for this job.")

                var output = downloaded
                // Remove stale fragment/temporary files for this job, but never touch the final output.
                folder.listFiles()?.filter {
                    it != output && it.name.contains(jobMarker) && isExtractorTemporaryFile(it.name)
                }?.forEach { it.delete() }
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

                // A single media job must leave only its final, playable output in Downloads.
                // This also removes any split audio/video tracks yt-dlp left beside the muxed file.
                folder.listFiles()?.filter {
                    it != output && it.name.contains(jobMarker)
                }?.forEach { it.delete() }

                val result = JSObject()
                result.put("filePath", output.absolutePath)
                result.put("fileName", output.name)
                result.put("format", args.kind)
                invoke.resolve(result)
            } catch (error: Exception) {
                invoke.reject(error.message ?: "Mobile media download failed.")
            } finally {
                cancelledGenerations.remove(args.jobId, generation)
            }
        }, "DownBest-media-download").start()
    }

    private fun isExtractorTemporaryFile(fileName: String): Boolean {
        val name = fileName.lowercase()
        return name.endsWith(".part") || name.contains(".part-frag") ||
            name.endsWith(".ytdl") || name.endsWith(".temp") || name.endsWith(".tmp")
    }

    private fun parseTotalBytes(line: String): Long? {
        val match = Regex("""(?:of\s+)(?:~)?([0-9]+(?:\.[0-9]+)?)\s*([KMGT]?i?B)""", RegexOption.IGNORE_CASE)
            .find(line) ?: return null
        val amount = match.groupValues[1].toDoubleOrNull() ?: return null
        val factor = byteUnitFactor(match.groupValues[2]) ?: return null
        return (amount * factor).toLong().takeIf { it > 0L }
    }

    private fun parseSpeedBytesPerSecond(line: String): Long? {
        val match = Regex("""(?:at\s+)?([0-9]+(?:\.[0-9]+)?)\s*([KMGT]?i?B)/s\b""", RegexOption.IGNORE_CASE)
            .find(line) ?: return null
        val amount = match.groupValues[1].toDoubleOrNull() ?: return null
        val factor = byteUnitFactor(match.groupValues[2]) ?: return null
        return (amount * factor).toLong().takeIf { it > 0L }
    }

    private fun byteUnitFactor(rawUnit: String): Double? = when (rawUnit.uppercase()) {
        "B" -> 1.0
        "KB" -> 1_000.0
        "MB" -> 1_000_000.0
        "GB" -> 1_000_000_000.0
        "TB" -> 1_000_000_000_000.0
        "KIB" -> 1024.0
        "MIB" -> 1024.0 * 1024.0
        "GIB" -> 1024.0 * 1024.0 * 1024.0
        "TIB" -> 1024.0 * 1024.0 * 1024.0 * 1024.0
        else -> null
    }

    private fun shellQuote(value: String): String =
        "'" + value.replace("'", "'\\''") + "'"
}
