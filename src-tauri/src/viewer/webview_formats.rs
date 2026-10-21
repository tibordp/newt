//! What the webview itself decodes: the images Image mode shows and the
//! audio and video the media player plays. A type outside these lists
//! opens in Structure instead, where the viewer can't show it.
//!
//! The webview is WebKit on macOS (ImageIO, AVFoundation), WebKitGTK on
//! Linux (its own image decoders, GStreamer for media, so playback also
//! depends on the plugins installed) and WebView2, i.e. Chromium, on
//! Windows. Each list follows its engine and changes with it. Types are
//! listed as fillyfoal identifies them, plus the aliases extension
//! databases use.

const IMAGES: &[&str] = &[
    "image/png",
    "image/apng",
    "image/jpeg",
    "image/gif",
    "image/webp",
    "image/avif",
    "image/bmp",
    "image/x-icon",
    "image/vnd.microsoft.icon",
    "image/svg+xml",
];

#[cfg(target_os = "macos")]
const IMAGES_HERE: &[&str] = &["image/tiff", "image/heic", "image/heif", "image/jxl"];
#[cfg(not(target_os = "macos"))]
const IMAGES_HERE: &[&str] = &[];

const AUDIO: &[&str] = &[
    "audio/mpeg",
    "audio/mp4",
    "audio/x-m4a",
    "audio/aac",
    "audio/wav",
    "audio/x-wav",
    "audio/wave",
    "audio/flac",
    "audio/x-flac",
    "audio/webm",
];

#[cfg(target_os = "macos")]
const AUDIO_HERE: &[&str] = &["audio/aiff", "audio/x-aiff"];
#[cfg(windows)]
const AUDIO_HERE: &[&str] = &["audio/ogg"];
#[cfg(target_os = "linux")]
const AUDIO_HERE: &[&str] = &["audio/aiff", "audio/x-aiff", "audio/ogg"];

const VIDEO: &[&str] = &["video/mp4", "video/webm"];

#[cfg(target_os = "macos")]
const VIDEO_HERE: &[&str] = &["video/quicktime", "video/x-m4v"];
#[cfg(windows)]
const VIDEO_HERE: &[&str] = &[];
#[cfg(target_os = "linux")]
const VIDEO_HERE: &[&str] = &["video/quicktime", "video/x-matroska", "video/ogg"];

/// Whether Image mode can show an image of type `mime`.
pub fn shows_image(mime: &str) -> bool {
    IMAGES.contains(&mime) || IMAGES_HERE.contains(&mime)
}

/// Whether the media player can play audio of type `mime`.
pub fn plays_audio(mime: &str) -> bool {
    AUDIO.contains(&mime) || AUDIO_HERE.contains(&mime)
}

/// Whether the media player can play video of type `mime`.
pub fn plays_video(mime: &str) -> bool {
    VIDEO.contains(&mime) || VIDEO_HERE.contains(&mime)
}
