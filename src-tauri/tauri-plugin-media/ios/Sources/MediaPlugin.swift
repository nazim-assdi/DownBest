import SwiftRs
import Tauri
import UIKit
import WebKit

class MediaDownloadArgs: Decodable {
    let url: String
    let destination: String
    let kind: String
    let concurrency: Int
}

class MediaPlugin: Plugin {
    @objc public func download(_ invoke: Invoke) throws {
        let args = try invoke.parseArgs(MediaDownloadArgs.self)
        invoke.reject(
            "On iOS, DownBest currently supports direct HTTP/HTTPS files and open, non-encrypted HLS playlists only. " +
            "Generic third-party site extraction is not enabled in the App Store build."
        )
    }
}

@_cdecl("init_plugin_downbest_media")
func initPlugin() -> Plugin {
    return MediaPlugin()
}
