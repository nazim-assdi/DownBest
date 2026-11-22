# DownBest Lite

**A low-CPU Rust + Tauri 2 download manager for Windows, macOS, Linux, Android and iOS.**

DownBest Lite combines a native async HTTP/HTTPS engine with desktop media extraction. Lite mode favours low CPU and memory pressure while keeping network throughput efficient; faster modes are available when the device has spare resources.

## Current implementation

- Lite mode is the default: 2 HTTP range connections, at most 2 simultaneous jobs, and an 8 MiB target chunk size to avoid excessive parallel work.
- Balanced mode uses 4 connections; High throughput mode uses 8. The server must support byte ranges for parallel downloads.
- A server capability probe, identity encoding for reliable byte offsets, and automatic single-connection fallback when range requests are unsupported or rejected.
- Multi-link batch submission: paste up to 50 HTTP/HTTPS URLs (one per line); all valid jobs enter the queue and up to 2 execute simultaneously while the rest wait in queue.
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
- App-specific private storage is used as the default destination on Android/iOS; the desktop folder picker is not shown on mobile.
- One Tauri 2 codebase and one Git branch: `main`.

## Supported targets and current limits

| Platform | Build package | Current download support |
|---|---|---|
| Windows | EXE / MSI | Direct HTTP(S) and desktop media extractors |
| macOS | DMG / APP | Direct HTTP(S) and desktop media extractors |
| Linux | AppImage / DEB / RPM | Direct HTTP(S) and desktop media extractors |
| Android | APK (ARM64 / x86_64) | Direct HTTP(S) and supported yt-dlp media sites |
| iOS | IPA | Direct HTTP(S) and open, unencrypted VOD HLS; Apple signing is required |

**Mobile limits:** Android bundles yt-dlp and FFmpegKit, so supported media-site extraction works without separate installations. The Android yt-dlp runtime is compiled for ARM64 and x86_64; it does not include 32-bit ABIs. iOS uses its native open-HLS saver only for finite, unencrypted VOD playlists; generic third-party site extraction is not enabled in the App Store build. Both platforms also support direct downloadable HTTP/HTTPS file URLs. The default mobile save path is inside the app's private storage. Exporting files into the public Downloads folder needs a native file-export / Storage Access Framework workflow and is not yet implemented.

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
- Android APKs split by ABI

The iOS job is disabled until Apple signing secrets are configured and the repository variable `ENABLE_SIGNED_IOS_BUILD=true` is set. Add these repository secrets:

- `APPLE_DEVELOPMENT_TEAM` — your Apple Developer Team ID.
- `IOS_CERTIFICATE` — base64-encoded Apple signing `.p12` certificate.
- `IOS_CERTIFICATE_PASSWORD` — password used when exporting the `.p12`.
- `IOS_MOBILE_PROVISION` — base64-encoded App Store provisioning profile for bundle ID `com.nazimassdi.downbest`.

The workflow will then export an App Store Connect IPA. Apple account enrollment, a valid distribution certificate and a matching active provisioning profile are required.

Packages are uploaded to the corresponding GitHub Actions run as workflow artifacts; they are not automatically published as a GitHub Release.

## Speed and reliability notes

Lite mode is recommended for everyday use on lower-powered PCs: it uses 2 requested HTTP connections, 2 simultaneous jobs, 256 KiB buffered disk writes, atomic progress updates, fewer idle pooled connections and a 1.5-second UI refresh while visible. Files below 16 MiB use a single connection; larger files may use up to 2 range connections. Balanced and High throughput modes can use 4 or 8 requested connections. These are limits, not guaranteed multipliers; actual throughput depends on ISP bandwidth, server/CDN limits, protocol, file size, disk and CPU. If a host rejects range requests, DownBest falls back to one stream.

The download queue and active job state are currently kept in memory; unfinished direct-file downloads do not yet survive an app restart. Automatic pause/resume from a persisted partial file is a later reliability milestone.

## Project status

The repository contains DownBest Lite 1.2.0, a cross-platform UI, a buffered Rust HTTP download core, desktop bundled media engines, Android native yt-dlp extraction, and iOS open-HLS support. Lite mode defaults to low CPU usage; Balanced and High throughput options are available. CI packaging has passed for the initial desktop builds; the latest mobile integration and bundled-tool builds are being tested. Physical Android/iOS device tests and signed iOS export are still required before claiming release readiness.
