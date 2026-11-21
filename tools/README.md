# Optional download tools

The Rust HTTP/HTTPS engine is built into DownBest and does not require external programs.

For desktop media-site extraction, install these platform-matching tools and expose them on the system PATH, or place them in a `tools/` folder beside the installed app executable:

- **yt-dlp** — supported media-site URL extraction.
- **aria2c** — optional external downloader for yt-dlp HTTP/FTP transfers.
- **FFmpeg** — merging separate video and audio streams.

Use official project releases and review their licence terms before redistributing binaries. This repository intentionally does not commit prebuilt third-party executables.

Android and iOS use the native Rust direct HTTP/HTTPS engine. The desktop yt-dlp/aria2c executables are not bundled into mobile builds.
