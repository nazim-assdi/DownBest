# DownBest

**A Rust + Tauri 2 download manager for Windows, macOS, Linux, Android and iOS.**

DownBest combines a native async HTTP/HTTPS engine with desktop media extraction. The goal is to use available bandwidth efficiently without promising a multiplier that the network or source server cannot deliver.

## Current implementation

- Adaptive parallel HTTP byte-range downloads (selected profile: 4, 8, 16 or 32 connections).
- A server capability probe, identity encoding for reliable byte offsets, and automatic single-connection fallback when range requests are unsupported or rejected.
- Up to three simultaneous download jobs; further jobs wait in the in-memory queue.
- Live byte counts, progress, average throughput, cancellation and queue cleanup.
- Desktop yt-dlp, aria2c and LGPL FFmpeg/FFprobe are fetched from upstream releases and bundled into Windows, macOS and Linux installers; no separate desktop tool installation is required.
- Desktop yt-dlp integration for supported media sites, generic HTML media pages and HLS/DASH manifests.
- Android bundles yt-dlp with an embedded Python runtime and FFmpegKit for audio-only output; it does not need a separate Python or yt-dlp install.
- iOS includes a native open-HLS VOD downloader for unencrypted playlists, plus the direct HTTP/HTTPS downloader.
- Audio-only, video-only and video+audio format selection for media-extractor links.
- yt-dlp concurrent fragment downloads; bundled aria2c is used for HTTP/FTP transfers while its native downloader remains selected for DASH/HLS.
- Bundled FFmpeg is used by yt-dlp to merge separate audio and video streams.
- App-specific private storage is used as the default destination on Android/iOS.
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

The connection selector is a maximum requested parallelism, not a promise that every host accepts that many connections. Actual throughput depends on ISP bandwidth, server/CDN limits, protocol, file size, disk and CPU. Small files use one stream because segmentation overhead would not help. If a host rejects range requests, DownBest retries using one stream.

This initial version keeps the download queue in memory; unfinished direct-file downloads do not yet survive an app restart. Automatic pause/resume from a persisted partial file is a later reliability milestone.

## Project status

The repository contains a cross-platform UI, a Rust HTTP download core, desktop bundled media engines, Android native yt-dlp extraction, and iOS open-HLS support. CI packaging has passed for the initial desktop builds; the latest mobile integration and bundled-tool builds are being tested. Physical Android/iOS device tests and signed iOS export are still required before claiming release readiness.
