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
import com.arthenica.ffmpegkit.FFmpegKit
import com.arthenica.ffmpegkit.ReturnCode
import java.io.File
import java.io.FileOutputStream
import java.util.concurrent.ConcurrentHashMap
import java.util.concurrent.Future
import java.util.concurrent.Executors
import java.util.zip.ZipInputStream
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
    private val mediaExecutor = Executors.newCachedThreadPool()
    private val pythonRuntimeLock = Any()
    @Volatile private var pythonRunnerReady = false
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
        val sourcePath = source.canonicalFile
        var target = File(folder, source.name)
        if (target.canonicalFile != sourcePath) {
            if (!target.createNewFile()) {
                val stem = source.nameWithoutExtension.ifBlank { "download" }
                val extension = source.extension
                var reserved: File? = null
                for (index in 1..10_000) {
                    val candidateName = if (extension.isBlank()) "$stem ($index)"
                        else "$stem ($index).$extension"
                    val candidate = File(folder, candidateName)
                    if (candidate.createNewFile()) {
                        reserved = candidate
                        break
                    }
                }
                target = reserved ?: throw IllegalStateException("Could not reserve a unique filename in app Downloads.")
            }
            try {
                source.inputStream().buffered().use { input ->
                    target.outputStream().buffered().use { output -> input.copyTo(output, 256 * 1024) }
                }
            } catch (error: Exception) {
                target.delete()
                throw error
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

    private fun copyToOpenCache(source: File): File {
        val folder = File(activity.cacheDir, "downbest-open")
        if (!folder.exists() && !folder.mkdirs()) {
            throw IllegalStateException("Could not prepare the file preview cache.")
        }
        val extension = source.extension.takeIf { it.matches(Regex("[A-Za-z0-9]{1,10}")) }
            ?.let { ".$it" } ?: ".bin"
        val target = File.createTempFile("download-", extension, folder)
        try {
            source.inputStream().buffered().use { input ->
                target.outputStream().buffered().use { output -> input.copyTo(output, 256 * 1024) }
            }
            require(target.isFile && target.length() > 0L) {
                "Could not prepare the downloaded file for opening."
            }
            return target
        } catch (error: Exception) {
            target.delete()
            throw error
        }
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
                    // Reuse Tauri's host FileProvider (already declared by the app).
                    // Copy into cache so the generated provider's cache-path is sufficient
                    // even when Android's app-specific Downloads folder is not exposed.
                    val shareFile = copyToOpenCache(file)
                    try {
                        FileProvider.getUriForFile(
                            activity,
                            "${activity.packageName}.fileprovider",
                            shareFile
                        )
                    } catch (error: Exception) {
                        shareFile.delete()
                        throw error
                    }
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

    private fun extractWheelAsset(assetName: String, destination: File) {
        val root = destination.canonicalFile
        ZipInputStream(activity.assets.open("python-wheels/$assetName").buffered()).use { zip ->
            var entry = zip.nextEntry
            while (entry != null) {
                val target = File(root, entry.name).canonicalFile
                require(target.path.startsWith(root.path + File.separator) || target == root) {
                    "The bundled Python wheel contains an invalid path."
                }
                if (entry.isDirectory) {
                    if (!target.exists() && !target.mkdirs()) {
                        throw IllegalStateException("Could not create a Python package directory.")
                    }
                } else {
                    target.parentFile?.let { if (!it.exists() && !it.mkdirs()) {
                        throw IllegalStateException("Could not create a Python package folder.")
                    } }
                    FileOutputStream(target).use { output -> zip.copyTo(output, 128 * 1024) }
                }
                zip.closeEntry()
                entry = zip.nextEntry
            }
        }
    }

    private fun preparePythonRunner() {
        ensureYtDlpInitialized()
        if (pythonRunnerReady) return
        synchronized(pythonRuntimeLock) {
            if (pythonRunnerReady) return
            val root = File(activity.filesDir, "downbest-python")
            val sitePackages = File(root, "site-packages")
            if (!sitePackages.exists() && !sitePackages.mkdirs()) {
                throw IllegalStateException("Could not prepare the Android Python runtime.")
            }

            val runnerFile = File(root, "downbest_ytdlp_runner.py")
            val runnerVersion = File(root, ".runner-version")
            if (!runnerFile.isFile || !runnerVersion.isFile ||
                runnerVersion.readText() != "3") {
                activity.assets.open("downbest_ytdlp_runner.py").buffered().use { input ->
                    runnerFile.outputStream().buffered().use { output -> input.copyTo(output, 64 * 1024) }
                }
                runnerVersion.writeText("3")
            }

            // Python 3.13 Android wheels are ABI-specific. If the wheel assets are
            // unavailable (e.g. a local developer build), yt-dlp still runs using
            // its standard networking stack.
            val abi = Build.SUPPORTED_ABIS.firstOrNull {
                it == "arm64-v8a" || it == "x86_64"
            }
            if (abi != null) {
                val platform = if (abi == "arm64-v8a") "android_24_arm64_v8a" else "android_24_x86_64"
                val wheelNames = try {
                    activity.assets.list("python-wheels")?.toList().orEmpty()
                } catch (_: Exception) {
                    emptyList()
                }
                val selectedWheels = wheelNames.filter { name ->
                    (name.startsWith("curl_cffi-0.16.3") ||
                        name.startsWith("cffi-2.1.1")) && name.endsWith("$platform.whl") ||
                        name.startsWith("pycparser-") && name.endsWith(".whl")
                }
                val marker = File(sitePackages, ".downbest-curl-cffi-0.16.3-$abi")
                val expected = if (selectedWheels.any { it.startsWith("curl_cffi-") } &&
                    selectedWheels.any { it.startsWith("cffi-") } &&
                    selectedWheels.any { it.startsWith("pycparser-") }) selectedWheels else emptyList()
                if (expected.isNotEmpty() && !marker.isFile) {
                    expected.forEach { extractWheelAsset(it, sitePackages) }
                    marker.writeText("installed")
                }
            }

            val pythonClass = Class.forName("com.chaquo.python.Python")
            val pyObjectClass = Class.forName("com.chaquo.python.PyObject")
            val python = pythonClass.getMethod("getInstance").invoke(null)
            val getModule = pythonClass.getMethod("getModule", String::class.java)
            val getAttribute = pyObjectClass.getMethod("get", Any::class.java)
            val callAttribute = pyObjectClass.getMethod(
                "callAttr", String::class.java, Array<Any>::class.java
            )
            val sys = getModule.invoke(python, "sys")
            val pythonPath = getAttribute.invoke(sys, "path")
            callAttribute.invoke(pythonPath, "insert", arrayOf<Any>(0, sitePackages.absolutePath))
            callAttribute.invoke(pythonPath, "insert", arrayOf<Any>(0, root.absolutePath))
            // Import once now so missing runtime or Python module errors surface
            // during Inspect/Download instead of becoming a silent empty response.
            getModule.invoke(python, "downbest_ytdlp_runner")
            pythonRunnerReady = true
        }
    }

    private fun pythonRunnerCall(function: String, vararg args: Any): String {
        preparePythonRunner()
        val pythonClass = Class.forName("com.chaquo.python.Python")
        val pyObjectClass = Class.forName("com.chaquo.python.PyObject")
        val python = pythonClass.getMethod("getInstance").invoke(null)
        val module = pythonClass.getMethod("getModule", String::class.java)
            .invoke(python, "downbest_ytdlp_runner")
        val callAttribute = pyObjectClass.getMethod(
            "callAttr", String::class.java, Array<Any>::class.java
        )
        val result = callAttribute.invoke(module, function, args)
        return result?.toString().orEmpty()
    }

    private fun javascriptRuntimePath(): String? {
        val abi = Build.SUPPORTED_ABIS.firstOrNull { it == "arm64-v8a" || it == "x86_64" }
            ?: return null
        val runtime = File(activity.applicationInfo.nativeLibraryDir, "libqjs.so")
        if (runtime.isFile && !runtime.canExecute()) runtime.setExecutable(true, false)
        return runtime.absolutePath.takeIf { runtime.isFile && runtime.canExecute() }
    }

    @Command
    fun inspect(invoke: Invoke) {
        val args = invoke.parseArgs(InspectArgs::class.java)
        Thread({
            try {
                require(args.url.startsWith("https://") || args.url.startsWith("http://")) {
                    "Only HTTP and HTTPS media links are supported."
                }
                val runtimePath = javascriptRuntimePath() ?: ""
                val output = pythonRunnerCall(
                    "inspect",
                    args.url,
                    args.flatPlaylist,
                    "",
                    runtimePath
                )
                require(output.isNotBlank()) {
                    "The Android media extractor returned empty metadata. Please update DownBest and retry."
                }
                val result = JSObject()
                result.put("output", output)
                result.put("errorOutput", "")
                result.put("exitCode", 0)
                invoke.resolve(result)
            } catch (error: Exception) {
                // Reflection wraps Python exceptions in one or more Java exceptions.
                // Unwrap to the real yt-dlp/Chaquopy cause so the UI can explain failures.
                var root = error as Throwable
                val visited = HashSet<Throwable>()
                while (root.cause != null && root.cause !== root && visited.add(root)) {
                    root = root.cause!!
                }
                val message = root.message?.takeIf { it.isNotBlank() } ?: root.toString()
                val runtimeInfo = if (pythonRunnerReady) {
                    try { pythonRunnerCall("get_capabilities") } catch (_: Exception) { "" }
                } else ""
                val detail = if (runtimeInfo.isNotBlank()) {
                    "$message (Android extractor: $runtimeInfo)"
                } else {
                    message
                }
                invoke.reject(detail)
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
                val cookiePath = if (!args.cookiesFile.isNullOrBlank()) {
                    val cookieFile = File(args.cookiesFile!!)
                    require(cookieFile.isFile && cookieFile.canRead()) {
                        "The imported cookies.txt file is no longer accessible. Import it again and retry."
                    }
                    cookieFile.absolutePath
                } else ""

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
                if (isCancelled(args.jobId, generation)) throw InterruptedException("Download cancelled.")
                val progressSpeedLock = Any()
                var lastProgressBytes = 0L
                var lastProgressAtNanos = System.nanoTime()
                var smoothedSpeedBytesPerSecond = 0L
                val progressCallback = DownloadProgressCallback { progress, eta, line ->
                    if (isCancelled(args.jobId, generation) || Thread.currentThread().isInterrupted) {
                        throw InterruptedException("Download cancelled.")
                    }
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
                }
                val future = mediaExecutor.submit<String> {
                    if (isCancelled(args.jobId, generation)) throw InterruptedException("Download cancelled.")
                    val runtimePath = javascriptRuntimePath() ?: ""
                    pythonRunnerCall(
                        "download",
                        args.url,
                        template,
                        selectedFormat,
                        args.concurrency.coerceIn(1, 16),
                        cookiePath,
                        runtimePath,
                        progressCallback
                    )
                }
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
                if (response != "OK") {
                    throw IllegalStateException(
                        response.takeLast(1800).ifBlank { "The Android media extractor did not complete the download." }
                    )
                }

                val jobMarker = " - ${args.jobId}."
                val downloaded = folder.listFiles()
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
                val wasCancelled = isCancelled(args.jobId, generation) ||
                    error is java.util.concurrent.CancellationException ||
                    Thread.currentThread().isInterrupted
                if (wasCancelled) {
                    invoke.reject("Download cancelled.")
                } else {
                    var root = error as Throwable
                    val visited = HashSet<Throwable>()
                    while (root.cause != null && root.cause !== root && visited.add(root)) {
                        root = root.cause!!
                    }
                    invoke.reject(root.message?.takeIf { it.isNotBlank() } ?: root.toString())
                }
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
