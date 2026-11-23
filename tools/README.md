# Download engine packaging

DownBest's CI workflow prepares platform-matched desktop engines before building installers. The user does not need to install these tools separately:

- **yt-dlp** — fetched from the official yt-dlp GitHub release for Windows, macOS and Linux.
- **aria2c** — fetched from the official Windows release, Homebrew on macOS (with dependent dylibs bundled), or Ubuntu packages on Linux (with non-system shared libraries bundled).
- **FFmpeg / FFprobe** — LGPL build from FFmpeg-Builds on Windows/Linux; a minimal LGPL-only static build from the FFmpeg 9.1 source release on macOS.
- **Deno 2.9.8** — pinned official release binaries for Windows x64, macOS arm64/x64 and Linux x64; used as yt-dlp's JavaScript challenge runtime.
- **yt-dlp EJS scripts** — fetched from the official yt-dlp remote component source when a supported extractor needs them.
- All tools are copied into `src-tauri/tools/`; Tauri includes that directory as bundle resources. The Rust backend searches this resource directory before falling back to the system PATH.

The desktop binaries are prepared at build time and are not committed to Git. Do not put executables for one OS into another OS's package.

## Mobile engines

- **Android:** The native Tauri plugin embeds `dev.ffmpegkit-maintained:yt-dlp-android:2.0.2` (Python 3.13 + yt-dlp), `dev.ffmpegkit-maintained:ffmpeg-kit-full:8.1.9` (LGPL FFmpegKit), and a QuickJS-NG v0.18.0 executable for each supported ABI. QuickJS is built by the Android NDK and packaged in the app's native library directory so yt-dlp can use it for YouTube EJS challenges. No Python, yt-dlp, JavaScript runtime or FFmpeg installation is required on the phone. The bundled yt-dlp AAR supports ARM64 and x86_64 only.
- **iOS:** The app keeps the native Rust direct-download engine and adds a native HLS VOD path for completed, unencrypted playlists. Generic third-party site extraction is not enabled in the App Store configuration. DRM, encrypted streams and live playlists are rejected.

See `THIRD_PARTY_NOTICES.md` for licenses and upstream source links.
