//! On-device OCR via Apple Vision (`VNRecognizeTextRequest`, accurate
//! mode). All objc2 unsafety is confined to this module behind safe fns;
//! the public surface is plain Rust types. The "model version" is Vision's
//! text-recognition request revision, pinned explicitly so results stay
//! reproducible across OS updates and encoded in [`OCR_MODEL_TAG`].

#[cfg(target_os = "macos")]
use std::io::Cursor;

#[cfg(target_os = "macos")]
use objc2::AnyThread as _;
#[cfg(target_os = "macos")]
use objc2::rc::Retained;
#[cfg(target_os = "macos")]
use objc2::runtime::AnyObject;
#[cfg(target_os = "macos")]
use objc2_foundation::{NSArray, NSData, NSDictionary, NSString};
#[cfg(target_os = "macos")]
use objc2_vision::{
    VNImageRequestHandler, VNRecognizeTextRequest, VNRecognizeTextRequestRevision3, VNRequest,
    VNRequestTextRecognitionLevel,
};

use crate::error::IndexError;

/// Whether this build can produce OCR derivations (Vision framework).
pub const AVAILABLE: bool = cfg!(target_os = "macos");

pub const OCR_MODEL_TAG: &str = "applevision-r3-v1";
/// Pinned `VNRecognizeTextRequest` revision; must agree with the `r3` in
/// [`OCR_MODEL_TAG`] (the request is configured from the bindings'
/// `VNRecognizeTextRequestRevision3`, which is this same value).
pub const OCR_REVISION: u32 = 3;

/// One recognized text line (Vision's top candidate for one observation).
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct OcrLine {
    pub text: String,
    pub confidence: f64,
    /// Normalized `[x, y, width, height]`, Vision's bottom-left origin.
    pub bbox: [f64; 4],
}

/// Recognized lines for one image at a pinned Vision revision.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct OcrResult {
    pub revision: u32,
    pub lines: Vec<OcrLine>,
}

impl OcrResult {
    /// # Errors
    /// Serialization failure (never expected for these plain types).
    pub fn to_json(&self) -> Result<Vec<u8>, IndexError> {
        serde_json::to_vec(self).map_err(|error| IndexError::Model(format!("ocr json: {error}")))
    }

    /// # Errors
    /// Returns `IndexError::Model` on malformed bytes.
    pub fn from_json(bytes: &[u8]) -> Result<Self, IndexError> {
        serde_json::from_slice(bytes)
            .map_err(|error| IndexError::Model(format!("ocr parse: {error}")))
    }
}

/// Recognize text in an image. Empty `lines` is a valid answer ("no text")
/// and is stored as such — otherwise the planner would retry forever.
///
/// # Errors
/// Returns [`IndexError::Encoder`] when Vision itself fails (not when it
/// simply finds nothing), or when the image can't be re-encoded to PNG for
/// handoff.
#[cfg(target_os = "macos")]
pub fn recognize_text(image: &image::RgbImage) -> Result<OcrResult, IndexError> {
    // Hand Vision an in-memory PNG via initWithData:options: — sidesteps
    // CGImage construction (and its pixel-format bookkeeping) entirely.
    let mut png = Vec::new();
    image
        .write_to(&mut Cursor::new(&mut png), image::ImageFormat::Png)
        .map_err(|error| IndexError::Encoder(format!("ocr png encode: {error}")))?;

    let request = VNRecognizeTextRequest::new();
    request.setRecognitionLevel(VNRequestTextRecognitionLevel::Accurate);
    // SAFETY: -[VNRequest setRevision:] only writes an NSUInteger property;
    // revision 3 (`VNRecognizeTextRequestRevision3`) is a supported revision
    // of this request class on every macOS this crate targets, and the
    // request is exclusively owned here, before the synchronous perform.
    unsafe { request.setRevision(VNRecognizeTextRequestRevision3) };

    let data = NSData::with_bytes(&png);
    let options: Retained<NSDictionary<NSString, AnyObject>> = NSDictionary::new();
    let handler = VNImageRequestHandler::initWithData_options(
        VNImageRequestHandler::alloc(),
        &data,
        &options,
    );

    let upcast: Retained<VNRequest> = Retained::into_super(Retained::into_super(request.clone()));
    let requests = NSArray::from_retained_slice(&[upcast]);
    // Synchronous: returns once the request has run; results land on the
    // retained `request` itself.
    handler
        .performRequests_error(&requests)
        .map_err(|error| IndexError::Encoder(format!("ocr vision perform: {error:?}")))?;

    let mut lines = Vec::new();
    if let Some(observations) = request.results() {
        for observation in &*observations {
            let Some(candidate) = observation.topCandidates(1).firstObject() else {
                continue;
            };
            // SAFETY: -[VNDetectedObjectObservation boundingBox] reads a
            // CGRect property by value from an observation we hold retained;
            // no aliasing or lifetime obligations beyond the receiver.
            let rect = unsafe { observation.boundingBox() };
            lines.push(OcrLine {
                text: candidate.string().to_string(),
                confidence: f64::from(candidate.confidence()),
                bbox: [
                    rect.origin.x,
                    rect.origin.y,
                    rect.size.width,
                    rect.size.height,
                ],
            });
        }
    }
    Ok(OcrResult {
        revision: OCR_REVISION,
        lines,
    })
}

/// Stub for builds without the Vision framework. Signature matches the
/// macOS version exactly so call sites compile everywhere.
///
/// # Errors
/// Always returns [`IndexError::PlatformUnavailable`] — OCR has no
/// non-Apple backend.
#[cfg(not(target_os = "macos"))]
pub fn recognize_text(_image: &image::RgbImage) -> Result<OcrResult, IndexError> {
    Err(IndexError::PlatformUnavailable {
        capability: "OCR",
        framework: "the Vision framework",
    })
}

#[cfg(test)]
mod tests {
    #[cfg(target_os = "macos")]
    use objc2_vision::VNRecognizeTextRequestRevision3;

    #[cfg(target_os = "macos")]
    use crate::ocr::{OCR_MODEL_TAG, OCR_REVISION};

    /// A Vision revision bump must not silently keep writing blobs under the
    /// old revision's model tag — the pinned revision, the binding's
    /// constant, and the tag must always agree.
    #[cfg(target_os = "macos")]
    #[test]
    fn ocr_revision_matches_the_vision_binding_constant() {
        let pinned = usize::try_from(OCR_REVISION).expect("u32 fits in NSUInteger");
        assert_eq!(pinned, VNRecognizeTextRequestRevision3);
        assert!(
            OCR_MODEL_TAG.contains(&format!("r{OCR_REVISION}")),
            "OCR_MODEL_TAG ({OCR_MODEL_TAG}) must encode OCR_REVISION ({OCR_REVISION})"
        );
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn the_ocr_stub_names_the_gap_and_the_framework() {
        let image = image::RgbImage::new(2, 2);
        let err = super::recognize_text(&image).expect_err("stub must refuse");
        let rendered = err.to_string();
        assert!(rendered.contains("OCR"));
        assert!(rendered.contains("Vision"));
        assert!(rendered.contains("macOS"));
    }
}
