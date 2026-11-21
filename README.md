# DownBest

**A Rust + Tauri 2 download manager for Windows, macOS, Linux, Android and iOS.**

DownBest combines a native async HTTP/HTTPS engine with desktop media extraction. The goal is to use available bandwidth efficiently without promising a multiplier that the network or source server cannot deliver.

## Current implementation

- Adaptive parallel HTTP byte-range downloads (selected profile: 4, 8, 16 or 32 connections).
- A server capability probe, identity encoding for reliable byte offsets, and automatic single-connection fallback when range requests are unsupported or rejected.
- Up to three simultaneous download jobs; further jobs wait in the in-memory queue.
- Live byte counts, progress, average throughput, cancellation and queue cleanup.
- Desktop yt-dlp integration for supported media sites, generic HTML media pages and HLS/DASH manifests.
- Audio-only, video-only and video+audio format selection for media-extractor links.
- yt-dlp concurrent fragment downloads; optional aria2c for HTTP/FTP transfers while its native downloader remains selected for DASH/HLS.
- FFmpeg is used by yt-dlp to merge separate audio and video streams when installed.
- App-specific private storage is used as the default destination on Android/iOS.
- One Tauri 2 codebase and one Git branch: `main`.

## Supported targets and current limits

| Platform | Build package | Current download support |
|---|---|---|
| Windows | EXE / MSI | Direct HTTP(S) and desktop media extractors |
| macOS | DMG / APP | Direct HTTP(S) and desktop media extractors |
| Linux | AppImage / DEB / RPM | Direct HTTP(S) and desktop media extractors |
| Android | APK | Direct HTTP(S) file links |
| iOS | IPA | Direct HTTP(S) file links; Apple signing is required |

**Mobile limitation:** yt-dlp, aria2c and FFmpeg are desktop executables in this first version. Media-site extraction (for example, YouTube pages) is desktop-only; direct downloadable HTTP/HTTPS file URLs work on mobile. The default mobile save path is inside the app's private storage. Exporting files into the public Downloads folder needs a native file-export / Storage Access Framework workflow and is not yet implemented.

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
    npm run tauri android build -- --apk

iOS (macOS + Xcode + Apple Developer signing configured):

    npm run tauri ios init
    npm run tauri ios build -- --export-method app-store-connect

## Optional desktop media tools

Install platform-matching releases and place them on PATH, or in a `tools/` folder beside the installed app executable:

- `yt-dlp` — media-site extraction.
- `aria2c` — optional accelerated HTTP/FTP transfers invoked by yt-dlp.
- `ffmpeg` — merge separate audio/video streams.

The native Rust direct-file downloader does not require these tools. Do not download binaries from untrusted mirrors; review upstream licences before redistributing third-party tools.

## GitHub Actions builds

The workflow runs on pushes to `main` and can also be started manually. It generates the platform icons and builds:

- Windows installer (.exe via NSIS)
- macOS DMG for Apple Silicon and Intel
- Linux AppImage and DEB
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

The repository started empty and now contains the first cross-platform scaffold, UI, Rust download core and CI workflow. Build jobs and real-device testing still need to pass before the installers can be called release-ready.
