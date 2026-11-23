# DownBest third-party notices

DownBest fetches platform-specific executable tools during release builds and packages them as separate child processes. These programs are not linked into the DownBest Rust executable.

## yt-dlp

- Purpose: supported media-site extraction on desktop and Android.
- Project: https://github.com/yt-dlp/yt-dlp
- License: The Unlicense (see the upstream repository's LICENSE file).
- Desktop downloads: official yt-dlp GitHub Releases. The exact released version is determined by the official `latest` release at build time.
- Android: `dev.ffmpegkit-maintained:yt-dlp-android:2.0.2`, which embeds Python 3.13 and yt-dlp. See https://github.com/ffmpegkit-maintained/yt-dlp-android and the runtime `THIRD-PARTY-NOTICES.txt` included in that Android library.

## aria2

- Purpose: optional multi-connection HTTP/FTP transfers invoked by desktop yt-dlp.
- Project and source: https://github.com/aria2/aria2
- Version used by desktop builds: 1.37.0.
- License: GNU GPL version 2 or later; see the upstream `COPYING` file.
- Windows uses the official upstream release archive. macOS/Linux use the platform package managers in CI.
- aria2 runs as a separate executable process; it is not statically or dynamically linked into DownBest.

## FFmpeg

- Purpose: merge separate audio/video streams on desktop; Android uses FFmpegKit for audio-only output conversion.
- Project and source: https://ffmpeg.org/
- Windows/Linux desktop builds use the LGPL variant of FFmpeg-Builds: https://github.com/FFmpeg-Builds/FFmpeg-Builds
- macOS desktop builds compile FFmpeg 9.1 from the official source archive with GPL and non-free components disabled and static libraries enabled.
- Android dependency: `dev.ffmpegkit-maintained:ffmpeg-kit-full:8.1.9`, LGPL v3. See https://github.com/ffmpegkit-maintained/ffmpeg and https://central.sonatype.com/artifact/dev.ffmpegkit-maintained/ffmpeg-kit-full.
- FFmpeg is distributed as a separate executable on desktop; the Android AAR's own licence and source obligations apply to the mobile binary.

## JavaScript challenge runtimes

- **Deno 2.9.8** — desktop JavaScript runtime used by yt-dlp for supported YouTube challenge solving. Project: https://github.com/denoland/deno. License: MIT. CI downloads the official platform-matched release binaries.
- **QuickJS-NG 0.18.0** — Android JavaScript runtime used by yt-dlp. Project: https://github.com/quickjs-ng/quickjs. License: MIT. CI builds ABI-matched executables from the official source archive with the Android NDK and packages them in the Android media plugin.
- **yt-dlp EJS remote component** — JavaScript challenge scripts are obtained via yt-dlp's official `ejs:github` remote component source. Project: https://github.com/yt-dlp/ejs. See the upstream repository for its current notices and source terms.

## Other dependencies

- Tauri, Rust crates and JavaScript dependencies retain their respective upstream licences; see each dependency's package metadata.
- The custom Tauri media plugin is distributed with this repository under the repository's applicable licence.

The CI tool preparation downloads official releases or uses platform package sources, rather than committing third-party executable files into Git. For exact versions, checksums and complete corresponding source, consult the upstream release referenced above and the build workflow.
