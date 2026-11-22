# DownBest Lite

**A low-CPU Rust + Tauri 2 download manager for Windows, macOS, Linux, Android and iOS.**

DownBest Lite combines a native async HTTP/HTTPS engine with desktop media extraction. Lite mode favours low CPU and memory pressure while keeping network throughput efficient; faster modes are available when the device has spare resources.

## Current implementation

- Lite mode is the default: 2 HTTP range connections, at most 2 simultaneous jobs, and an 8 MiB target chunk size to avoid excessive parallel work.
- Balanced mode uses 4 connections; High throughput mode uses 8. The server must support byte ranges for parallel downloads.
- A server capability probe, identity encoding for reliable byte offsets, and automatic single-connection fallback when range requests are unsupported or rejected.
- Multi-link batch submission: paste up to 50 HTTP/HTTPS URLs (one per line); all valid jobs enter the queue and up to 2 execute simultaneously while the rest wait in queue. Concurrent downloads reserve unique output filenames so same-name files do not overwrite each other.
- Atomic progress counters avoid a shared mutex on every received network chunk.
- 256 KiB buffered disk writes reduce small write operations; the UI refreshes active downloads every 650 ms and slows down while idle or hidden.
- Eye Comfort is the default UI theme, with Midnight and Soft Light options, locally persisted appearance preferences, reduced motion and larger text controls; the interface uses system fonts and works without a remote font request.
- Desktop yt-dlp, aria2c and LGPL FFmpeg/FFprobe are fetched from upstream releases and bundled into Windows, macOS and Linux installers; no separate desktop tool installation is required.
- Desktop yt-dlp integration for supported media sites, generic HTML media pages and HLS/DASH manifests. Both process output streams are drained concurrently: stderr drives live progress and stdout reports the final post-processed file path. Windows extractor processes launch without a visible console window.
- Android bundles yt-dlp with an embedded Python runtime and FFmpegKit for audio-only output; it does not need a separate Python or yt-dlp install.
- iOS includes a native open-HLS VOD downloader for unencrypted playlists, plus the direct HTTP/HTTPS downloader.
- Audio-only, video-only and video+audio format selection for media-extractor links.
- yt-dlp concurrent fragment downloads; bundled aria2c is used for HTTP/FTP transfers while its native downloader remains selected for DASH/HLS.
- Bundled FFmpeg is used by yt-dlp to merge separate audio and video streams.
- Desktop users can choose the save folder with the native folder picker and reveal completed files in the system file manager. The default folder is Downloads.
- Desktop downloads default directly to the operating system Downloads directory, without a DownBest subfolder. Android uses private staging during a transfer, then publishes completed files into the shared system Downloads collection through MediaStore. iOS retains app-private storage.
- Persistent download queue stored in the app-data folder; unfinished jobs return as Paused after an app/PC restart and can be resumed from the queue.
- Direct HTTP(S) downloads keep durable partial files, validate byte-range offsets, use ETag/Last-Modified validators when available, and sync partial data to disk periodically.
- Desktop yt-dlp downloads use a stable per-job output template, `--continue`, `--part` and `--keep-fragments`; resumable fragments are retained when yt-dlp and the source extractor support them.
- Android yt-dlp downloads use the same stable job ID and resumable .part/fragment flags. The active extractor future can be interrupted by Pause, then re-run against the same partial files after restart.
- One Tauri 2 codebase and one Git branch: `main`.

## Supported targets and current limits

| Platform | Build package | Current download support |
|---|---|---|
| Windows | EXE / MSI | Direct HTTP(S) and desktop media extractors |
| macOS | DMG / APP | Direct HTTP(S) and desktop media extractors |
| Linux | AppImage / DEB / RPM | Direct HTTP(S) and desktop media extractors |
| Android | APK (ARM64 / x86_64) | Direct HTTP(S) and supported yt-dlp media sites |
| iOS | IPA | Direct HTTP(S) and open, unencrypted VOD HLS; Apple signing is required |

**Mobile limits:** Android bundles yt-dlp and FFmpegKit, so supported media-site extraction works without separate installations. The Android yt-dlp runtime is compiled for ARM64 and x86_64; it does not include 32-bit ABIs. Finished Android files are copied into the shared Downloads folder through MediaStore on Android 10 and later; incomplete transfer data remains in app-private staging to preserve Pause/Resume. iOS uses its native open-HLS saver only for finite, unencrypted VOD playlists; generic third-party site extraction is not enabled in the App Store build, and its files remain in app-private storage.

yt-dlp supports many extractors, but no application can guarantee that every website, DRM-protected stream, login-gated media item or regional restriction will work. DownBest does not bypass DRM, authentication, access controls or source-server rate limits.

## Build locally

Install Node.js 22+, Rust stable and the system prerequisites for your host OS from the Tauri 2 documentation.

Generate the required platform icons from the included SVG source:

    npm install
    npm run tauri icon app-icon.svg

Run the development app:

    npm run tauri dev

Build a desktop release:

    npm run tauri build

Android (configured Android SDK/NDK and Java 17):

    npm run tauri android init
    npm run tauri icon app-icon.svg
    npm run tauri android build -- --apk

iOS (macOS + Xcode + Apple Developer signing configured):

    npm run tauri ios init
    npm run tauri icon app-icon.svg
    npm run tauri ios build -- --export-method app-store-connect

## Optional desktop media tools

Desktop releases now bundle platform-matching `yt-dlp`, `aria2c`, `ffmpeg` and `ffprobe` binaries in the Tauri resources directory. The app still detects a system-installed copy as a fallback when running a development build. Build automation fetches yt-dlp from its official GitHub releases, aria2 from its official release archive, and LGPL FFmpeg builds (Linux/Windows) or compiles a minimal LGPL-only FFmpeg from source on macOS. License notices and upstream source links are kept in `THIRD_PARTY_NOTICES.md`. The Android APK embeds yt-dlp-android and FFmpegKit through Maven Central; those libraries are pinned to versions in the Android plugin Gradle file.

## GitHub Actions builds

The workflow runs on pushes to `main` and can also be started manually. It generates the platform icons and builds:

- Windows EXE (NSIS) and MSI
- macOS DMG for Apple Silicon and Intel
- Linux AppImage, DEB and RPM
- Android APKs split by ABI (signed and verified for direct installation)

The Android CI step zip-aligns each APK, signs it with APK Signature Scheme support, verifies the signature and package manifest, checks the ABI-specific Rust library, and refuses to upload unsigned APKs. Without a persistent signing key, CI creates a temporary test-signing key for that workflow run; a later run will have a different certificate, so uninstall the previous CI build before installing the next one. For repeatable in-place app updates, configure these repository secrets with your own private Android release keystore:

- `DOWNBEST_ANDROID_KEYSTORE_BASE64` — base64-encoded keystore file.
- `DOWNBEST_ANDROID_STORE_PASSWORD` — keystore password.
- `DOWNBEST_ANDROID_KEY_ALIAS` — signing key alias.
- `DOWNBEST_ANDROID_KEY_PASSWORD` — signing key password.

Keep the keystore private and back it up securely. Do not commit it to the repository.

The iOS job is disabled until Apple signing secrets are configured and the repository variable `ENABLE_SIGNED_IOS_BUILD=true` is set. Add these repository secrets:

- `APPLE_DEVELOPMENT_TEAM` — your Apple Developer Team ID.
- `IOS_CERTIFICATE` — base64-encoded Apple signing `.p12` certificate.
- `IOS_CERTIFICATE_PASSWORD` — password used when exporting the `.p12`.
- `IOS_MOBILE_PROVISION` — base64-encoded App Store provisioning profile for bundle ID `com.nazimassdi.downbest`.

The workflow will then export an App Store Connect IPA. Apple account enrollment, a valid distribution certificate and a matching active provisioning profile are required.

Packages are uploaded to the corresponding GitHub Actions run as workflow artifacts; they are not automatically published as a GitHub Release.

## Speed and reliability notes

Lite mode is recommended for everyday use on lower-powered PCs: it uses 2 requested HTTP connections, 2 simultaneous jobs, 256 KiB buffered disk writes, atomic progress updates, fewer idle pooled connections and a 1.5-second UI refresh while visible. Files below 16 MiB use a single connection; larger files may use up to 2 range connections. Balanced and High throughput modes can use 4 or 8 requested connections. These are limits, not guaranteed multipliers; actual throughput depends on ISP bandwidth, server/CDN limits, protocol, file size, disk and CPU. If a host rejects range requests, DownBest falls back to one stream.

The queue is saved locally in the application's data directory. Running or queued jobs are restored as Paused after a restart; choose Resume on each item to continue. Direct HTTP(S) downloads resume from partial files when the source accepts byte-range requests and returns the expected range offsets; ETag/Last-Modified validators are used where supplied. If a server disables byte ranges, it is technically impossible to fetch only the missing suffix; DownBest safely restarts that file from zero. Desktop and Android yt-dlp use stable per-job output names and retain resumable .part/fragment data. Successful resumption still depends on the site's yt-dlp extractor, source URL/token validity, and the downloader library's support. iOS supports checkpointed resume for its supported finite, unencrypted VOD HLS layout: each fully written media segment is synced and recorded in a sidecar checkpoint, and a changed playlist signature causes a safe restart. CI now runs unsigned Rust compile checks for iOS device and simulator targets; this verifies compilation but does not replace a signed IPA or real-device test. Live, encrypted, byte-range, changing-init-map and unsupported playlist layouts remain outside the mobile HLS saver. A restart-recovery unit test verifies that a queued/running record and its durable partial bytes reappear as Paused in a fresh manager. This is a process-restart simulation, not a physical power-cut test on Windows hardware.

## Download location and cancellation

When no custom save folder is selected, desktop builds save directly to the operating system Downloads directory; no extra `DownBest` folder is created. On Android, transfers run in private staging and completed files are then published to the shared system `Download` directory through Android MediaStore, so normal file managers and the Downloads app can find them. This public-folder publishing path requires Android 10 or later. iOS continues to keep files in the app's private storage area.

Active jobs offer two separate stop actions. Pause preserves the HTTP range parts or extractor partial files and lets the job resume later. Cancel stops the job permanently, removes its incomplete HTTP/segment/extractor files, and records a terminal Cancelled state. Cancelled jobs do not show Resume. A Cancelled queue record is restored as Cancelled after app restart.

## Social-media and website downloads

Desktop downloads use the bundled yt-dlp engine for supported media pages, and Android uses the embedded yt-dlp runtime. Known social domains such as Facebook, Instagram, Threads, TikTok, X, Twitch, Reddit, Vimeo and other extractor sites are routed to that engine; ordinary HTML video pages also use yt-dlp's generic extractor. The extractor catalogue changes over time, and a site being listed does not guarantee that every URL will work. Check the upstream [yt-dlp supported-site list](https://github.com/yt-dlp/yt-dlp/blob/master/supportedsites.md).

Android's embedded free yt-dlp runtime supports a broad extractor catalogue, but does not include TLS fingerprint impersonation. Some sites may still reject requests due to bot checks, account restrictions, geography, age gates, changed extractors or other server rules. Android packages are restricted to `arm64-v8a` and `x86_64`; 32-bit Android devices are not supported by the bundled runtime. Generic third-party site extraction is not enabled on iOS; that build supports direct HTTP/HTTPS files and its supported open HLS mode.

## Project status

The repository contains DownBest Lite 1.2.0, a cross-platform UI, a buffered Rust HTTP download core, desktop bundled media engines, Android native yt-dlp extraction, and iOS open-HLS support. Lite mode defaults to low CPU usage; Balanced and High throughput options are available. CI packaging has passed for the initial desktop builds; the latest mobile integration and bundled-tool builds are being tested. Physical Android/iOS device tests and signed iOS export are still required before claiming release readiness.
