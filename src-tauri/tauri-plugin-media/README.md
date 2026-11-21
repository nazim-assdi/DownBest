# DownBest mobile media plugin

Android bundles yt-dlp with an embedded Python runtime. The AAR is pinned to a
version in the Gradle build; updating the library updates its bundled extractor.
The Android plugin also bundles an LGPL FFmpegKit runtime for audio processing.

iOS currently uses the Rust direct downloader and a separate open-HLS path.
Generic third-party site extraction is intentionally not enabled in the App Store
configuration because Apple's review rules prohibit unauthorized third-party media
downloading. This is not a DRM or access-control bypass.
