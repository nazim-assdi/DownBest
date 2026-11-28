"""DownBest's direct Chaquopy runner.

The stock yt-dlp-android wrapper only exposes an exit code and silently ignores
many CLI flags. This module uses yt-dlp's Python API directly so metadata JSON,
JS runtimes, extractor arguments, cookies, progress hooks and curl-cffi
impersonation are actually available on Android.
"""
from __future__ import annotations

import json
import os
from typing import Any

from yt_dlp import YoutubeDL


def _format_bytes(value: Any) -> str:
    if value is None:
        return "0B"
    try:
        number = float(value)
    except (TypeError, ValueError):
        return "0B"
    if number < 0:
        return "0B"
    units = ("B", "KiB", "MiB", "GiB", "TiB")
    for unit in units:
        if number < 1024 or unit == units[-1]:
            return f"{number:.1f}{unit}"
        number /= 1024
    return f"{number:.1f}TiB"


def _base_options(cookie_file: str = "", quickjs_path: str = "",
                 concurrency: int = 8) -> dict[str, Any]:
    options: dict[str, Any] = {
        "quiet": True,
        "no_warnings": True,
        "noplaylist": True,
        # Mobile networks often have variable latency; preserve a useful analysis
        # window before returning a precise timeout to the UI.
        "socket_timeout": 35,
        "retries": 5,
        "fragment_retries": 5,
        "extractor_retries": 3,
        "file_access_retries": 2,
        "concurrent_fragment_downloads": max(1, min(int(concurrency or 8), 16)),
        "buffersize": 1024 * 1024,
        "continuedl": True,
        "remote_components": ["ejs:github"],
        "extractor_args": {
            "youtube": {
                "player_client": ["web_safari", "tv_simply", "android"],
            },
        },
    }

    if cookie_file and os.path.isfile(cookie_file):
        options["cookiefile"] = cookie_file

    if quickjs_path and os.path.isfile(quickjs_path):
        options["js_runtimes"] = {"quickjs": {"path": quickjs_path}}

    # curl-cffi is supplied as an Android CPython 3.13 wheel by the build.
    # Keep normal yt-dlp networking as a fallback on local/dev builds without it.
    try:
        import curl_cffi  # noqa: F401
        options["impersonate"] = "chrome"
    except Exception:
        pass

    return options


def get_capabilities() -> str:
    try:
        import yt_dlp
        yt_dlp_version = getattr(yt_dlp, "__version__", "unknown")
    except Exception as error:
        yt_dlp_version = f"unavailable: {error}"
    try:
        import curl_cffi
        curl_version = getattr(curl_cffi, "__version__", "available")
    except Exception as error:
        curl_version = f"unavailable: {error}"
    return json.dumps({
        "yt_dlp_version": yt_dlp_version,
        "curl_cffi": curl_version,
    }, ensure_ascii=False)


def inspect(url: str, flat_playlist: bool = True,
            cookie_file: str = "", quickjs_path: str = "") -> str:
    options = _base_options(cookie_file, quickjs_path)
    if flat_playlist:
        options["extract_flat"] = "in_playlist"
        options["noplaylist"] = False
    else:
        options["noplaylist"] = True

    with YoutubeDL(options) as downloader:
        info = downloader.extract_info(url, download=False)
        if info is None:
            raise RuntimeError("The media extractor returned no information.")
        return json.dumps(
            downloader.sanitize_info(info),
            ensure_ascii=False,
            separators=(",", ":"),
        )


def download(url: str, output_template: str, format_selector: str,
             concurrency: int = 8, cookie_file: str = "",
             quickjs_path: str = "", progress_callback: Any = None) -> str:
    options = _base_options(cookie_file, quickjs_path, concurrency)
    options["outtmpl"] = output_template
    options["format"] = format_selector
    options["noplaylist"] = True
    options["progress_hooks"] = []

    if progress_callback is not None:
        def progress_hook(data: dict[str, Any]) -> None:
            if data.get("status") != "downloading":
                return
            try:
                percent = data.get("_percent_str", "0%")
                if isinstance(percent, str):
                    percent = percent.replace("%", "").strip()
                percent_value = float(percent or 0)
                eta = int(data.get("eta") or 0)
                total = data.get("total_bytes") or data.get("total_bytes_estimate")
                speed = data.get("speed")
                line = (
                    f"[download] {percent_value:.1f}% of {_format_bytes(total)} "
                    f"at {_format_bytes(speed)}/s"
                )
                progress_callback.onProgressUpdate(percent_value, eta, line)
            except Exception:
                # Progress UI must never abort the actual transfer.
                pass

        options["progress_hooks"].append(progress_hook)

    with YoutubeDL(options) as downloader:
        result = downloader.download([url])
    if result:
        raise RuntimeError(f"yt-dlp returned exit code {result}")
    return "OK"
