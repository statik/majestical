//! File classification by extension, shared by the index planner and the
//! `kind:` search filter so both always agree.

/// Coarse media class of a catalog path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum MediaKind {
    Image,
    Video,
    Audio,
    Pdf,
    Other,
}

/// Single source of truth for extension -> kind, so adding a format never
/// means touching more than one table.
const EXTENSIONS: &[(&str, MediaKind)] = &[
    // image
    ("jpg", MediaKind::Image),
    ("jpeg", MediaKind::Image),
    ("png", MediaKind::Image),
    ("gif", MediaKind::Image),
    ("tif", MediaKind::Image),
    ("tiff", MediaKind::Image),
    ("bmp", MediaKind::Image),
    ("webp", MediaKind::Image),
    ("heic", MediaKind::Image),
    ("heif", MediaKind::Image),
    ("avif", MediaKind::Image),
    ("dng", MediaKind::Image),
    ("cr2", MediaKind::Image),
    ("cr3", MediaKind::Image),
    ("nef", MediaKind::Image),
    ("arw", MediaKind::Image),
    ("raf", MediaKind::Image),
    ("orf", MediaKind::Image),
    ("rw2", MediaKind::Image),
    ("jxl", MediaKind::Image),
    ("pef", MediaKind::Image),
    ("iiq", MediaKind::Image),
    ("3fr", MediaKind::Image),
    // video
    ("mov", MediaKind::Video),
    ("mp4", MediaKind::Video),
    ("m4v", MediaKind::Video),
    ("avi", MediaKind::Video),
    ("mkv", MediaKind::Video),
    ("mxf", MediaKind::Video),
    ("mts", MediaKind::Video),
    ("m2ts", MediaKind::Video),
    ("webm", MediaKind::Video),
    ("r3d", MediaKind::Video),
    ("braw", MediaKind::Video),
    ("mpg", MediaKind::Video),
    ("mpeg", MediaKind::Video),
    ("3gp", MediaKind::Video),
    ("wmv", MediaKind::Video),
    ("insv", MediaKind::Video),
    // audio
    ("wav", MediaKind::Audio),
    ("mp3", MediaKind::Audio),
    ("m4a", MediaKind::Audio),
    ("aac", MediaKind::Audio),
    ("flac", MediaKind::Audio),
    ("aif", MediaKind::Audio),
    ("aiff", MediaKind::Audio),
    ("caf", MediaKind::Audio),
    ("ogg", MediaKind::Audio),
    // pdf
    ("pdf", MediaKind::Pdf),
];

impl MediaKind {
    /// Every variant, in declaration order — the single source of truth for
    /// "which kinds exist" so callers (e.g. the CLI's `kind:` filter) can
    /// derive their valid-value set instead of hand-listing it and risking
    /// drift when a variant is added.
    pub const ALL: [MediaKind; 5] = [
        MediaKind::Image,
        MediaKind::Video,
        MediaKind::Audio,
        MediaKind::Pdf,
        MediaKind::Other,
    ];

    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            MediaKind::Image => "image",
            MediaKind::Video => "video",
            MediaKind::Audio => "audio",
            MediaKind::Pdf => "pdf",
            MediaKind::Other => "other",
        }
    }
}

/// Classify a path (any base) by its extension, case-insensitively.
#[must_use]
pub fn media_kind(path: &str) -> MediaKind {
    let ext = path
        .rsplit('/')
        .next()
        .and_then(|name| name.rsplit_once('.'))
        .map(|(_, e)| e.to_ascii_lowercase());
    let Some(ext) = ext else {
        return MediaKind::Other;
    };
    EXTENSIONS
        .iter()
        .find(|(candidate, _)| *candidate == ext)
        .map_or(MediaKind::Other, |(_, kind)| *kind)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_by_extension_case_insensitively() {
        assert_eq!(media_kind("Clips/Beach.MOV"), MediaKind::Video);
        assert_eq!(media_kind("a/b/photo.jpeg"), MediaKind::Image);
        assert_eq!(media_kind("IMG_0001.HEIC"), MediaKind::Image);
        assert_eq!(media_kind("notes.txt"), MediaKind::Other);
        assert_eq!(media_kind("no_extension"), MediaKind::Other);
    }

    #[test]
    fn audio_and_pdf_kinds_classify() {
        assert_eq!(media_kind("voice-memo.m4a"), MediaKind::Audio);
        assert_eq!(media_kind("PODCAST.WAV"), MediaKind::Audio);
        assert_eq!(media_kind("brief.pdf"), MediaKind::Pdf);
        assert_eq!(media_kind("shot.mpg"), MediaKind::Video);
        assert_eq!(media_kind("frame.jxl"), MediaKind::Image);
        assert_eq!(media_kind("notes.txt"), MediaKind::Other);
    }

    #[test]
    fn all_lists_every_kind() {
        assert_eq!(MediaKind::ALL.len(), 5);
    }
}
