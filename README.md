# DownBest

**A Rust + Tauri 2 download manager for Windows, macOS, Linux, Android and iOS.**

DownBest is designed to use your available connection efficiently. Its direct-file engine uses adaptive HTTP byte-range downloads, while supported media-site links are routed through yt-dlp on desktop.

## Current implementation

- Rust async HTTP/HTTPS downloader with parallel byte-range segments (1–32 connections, selected by the user).
- Automatic fallback to a normal stream when the server does not support HTTP range requests.
- Up to three simultaneous download jobs, with additional jobs queued.
- Live progress, byte counts, average speed, cancellation and completed-item cleanup.
- Desktop media extraction through yt-dlp: audio-only, video with audio, and video-only modes.
- yt-dlp concurrent DASH/HLS fragments; aria2c is selected for supported HTTP/FTP transfers when installed.
- FFmpeg is used by yt-dlp to merge separate video/audio streams when available.
- One Tauri 2 codebase and one Git branch: main.

## Supported targets

| Target | Package goal | Current engine coverage |
|---|---|---|
| Windows | EXE / MSI | Direct HTTP(S), yt-dlp/aria2c/FFmpeg when installed |
| macOS | DMG / APP | Direct HTTP(S), yt-dlp/aria2c/FFmpeg when installed |
| Linux | AppImage / DEB / RPM | Direct HTTP(S), yt-dlp/aria2c/FFmpeg when installed |
| Android | APK | Direct HTTP(S) links |
| iOS | IPA | Direct HTTP(S) links; Apple signing setup required |

**Important mobile limitation:** yt-dlp and aria2c are desktop executables in this first implementation. YouTube and similar media-site extraction is therefore desktop-only. Direct downloadable HTTP/HTTPS URLs work on Android and iOS. iOS device distribution requires Apple Developer signing credentials and a provisioning profile.

## Build locally

Install Node.js 22+, Rust stable, and the platform prerequisites in the Tauri 2 documentation.

    npm install
    npm run tauri dev

Release build for desktop:

    npm run tauri build

Android (on a configured Android SDK/NDK host):

    npm run tauri android init
    npm run tauri android build -- --apk

iOS (on macOS with Xcode and signing configured):

    npm run tauri ios init
    npm run tauri ios build -- --export-method development

## Optional media tools

Install yt-dlp and optionally aria2c and FFmpeg, then make them available on PATH or place them in a tools directory beside the app executable.

- yt-dlp — media-site URL extraction.
- aria2c — multi-connection HTTP/FTP transfers invoked by yt-dlp when available.
- FFmpeg — merging separate audio/video streams.

The Rust HTTP engine does not require aria2c. It performs byte-range segmentation itself when the origin server supports it.

## Build artifacts

The GitHub Actions workflow runs on pushes to main and can be started manually. Desktop packages and Android APKs are uploaded as workflow artifacts. The signed iOS job is opt-in: set the repository variable ENABLE_SIGNED_IOS_BUILD=true and configure Apple Developer signing first.

## Speed expectations

No download manager can promise a fixed multiplier. Actual throughput is limited by ISP bandwidth, server-side connection limits, CDN behavior, protocol support, disk speed and CPU. DownBest caps parallel range requests at the selected profile and falls back when the server refuses ranges; it does not attempt to bypass access controls or rate limits.

## Project status

This repository started empty. This is the first application scaffold; it is not a claim that all platform installers or every media website have already been tested. CI build results and real-device tests are the source of truth for release readiness.
