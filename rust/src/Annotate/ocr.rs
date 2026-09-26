//! Port of `Annotate/OCR.swift`.
//!
//! On-device text recognition, Chinese + English. Lines come back
//! top-to-bottom (Vision's order is mostly reading order already; we sort
//! by top edge, then left edge).

use objc2::rc::Retained;
use objc2_core_graphics::CGImage;
use objc2_foundation::{NSArray, NSString};
use objc2_vision::{
    VNImageRequestHandler, VNRecognizeTextRequest, VNRecognizedTextObservation,
    VNRequestTextRecognitionLevel,
};

/// `OCR.recognize(_:)` — `Result` maps Swift's `throws`.
pub fn recognize(image: &CGImage) -> Result<String, String> {
    let request = VNRecognizeTextRequest::new();
    request.setRecognitionLevel(VNRequestTextRecognitionLevel::Accurate);
    request.setRecognitionLanguages(&NSArray::from_retained_slice(&[
        NSString::from_str("zh-Hans"),
        NSString::from_str("zh-Hant"),
        NSString::from_str("en-US"),
    ]));
    request.setUsesLanguageCorrection(true);
    let mtm = objc2::MainThreadMarker::new().expect("main thread");
    let handler = unsafe {
        VNImageRequestHandler::initWithCGImage_options(
            mtm.alloc(),
            image,
            &objc2_foundation::NSDictionary::new(),
        )
    };
    let requests = NSArray::from_retained_slice(&[request.clone()]);
    // SAFETY: VNRecognizeTextRequest is a VNRequest; only the element type
    // is re-marked.
    let requests: Retained<NSArray<objc2_vision::VNRequest>> =
        unsafe { Retained::cast_unchecked(requests) };
    if let Err(err) = handler.performRequests_error(&requests) {
        return Err(err.localizedDescription().to_string());
    }
    let observations = request.results().unwrap_or_else(NSArray::new);
    let mut list: Vec<Retained<VNRecognizedTextObservation>> = Vec::new();
    for o in observations.iter() {
        list.push(o);
    }
    // Vision's order is mostly reading order already; sort by top edge,
    // then left edge.
    list.sort_by(|a, b| {
        let (ar, br) = (unsafe { a.boundingBox() }, unsafe { b.boundingBox() });
        let (ay, by) = (
            ar.origin.y + ar.size.height,
            br.origin.y + br.size.height,
        );
        if (ay - by).abs() > 0.01 {
            by.partial_cmp(&ay).unwrap_or(std::cmp::Ordering::Equal)
        } else {
            ar.origin.x.partial_cmp(&br.origin.x).unwrap_or(std::cmp::Ordering::Equal)
        }
    });
    let mut lines: Vec<String> = Vec::new();
    for o in &list {
        if let Some(cands) = o.topCandidates(1).firstObject() {
            let s = cands.string().to_string();
            if !s.is_empty() {
                lines.push(s);
            }
        }
    }
    Ok(lines.join("\n"))
}
