//! File classification by extension, shared by the index planner and the
//! `kind:` search filter so both always agree.

/// Coarse media class of a catalog path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaKind {
    Image,
    Video,
    Other,
}

const IMAGE_EXTS: &[&str] = &[
    "jpg", "jpeg", "png", "gif", "tif", "tiff", "bmp", "webp", "heic", "heif", "avif", "dng",
    "cr2", "cr3", "nef", "arw", "raf", "orf", "rw2",
];
const VIDEO_EXTS: &[&str] = &[
    "mov", "mp4", "m4v", "avi", "mkv", "mxf", "mts", "m2ts", "webm", "r3d", "braw",
];

impl MediaKind {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            MediaKind::Image => "image",
            MediaKind::Video => "video",
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
    if IMAGE_EXTS.contains(&ext.as_str()) {
        MediaKind::Image
    } else if VIDEO_EXTS.contains(&ext.as_str()) {
        MediaKind::Video
    } else {
        MediaKind::Other
    }
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
}
