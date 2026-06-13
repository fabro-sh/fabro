//! Resolve file-backed attachments to inline data before a codec encodes.
//!
//! Codec `encode` is sync and never touches the filesystem, so any
//! `Image`/`Document`/`Audio` part whose `url` is a local file path is loaded
//! here (async) and rewritten to inline bytes + MIME, per the codec's policy.
//! Loads that fail drop the part silently — the long-standing contract — and
//! non-file URLs and already-inline data pass through untouched.
//!
//! Shared infra for the per-dialect codecs (anthropic/openai_responses/gemini):
//! each constructs its own [`AttachmentPolicy`] and calls [`resolve`] from its
//! adapter shell.

use std::borrow::Cow;

use crate::providers::common;
use crate::types::{AudioData, ContentPart, DocumentData, ImageData, Request};

/// Which attachment kinds a codec loads from local file paths. Each dialect
/// adapter constructs the policy it wants (e.g. images + documents but not
/// audio for Anthropic, which renders audio as a text placeholder).
#[derive(Clone, Copy)]
pub(crate) struct AttachmentPolicy {
    pub images:    bool,
    pub documents: bool,
    pub audio:     bool,
}

/// Resolve file-path attachments (per `policy`) to inline data. Parts whose
/// file fails to load are dropped. Borrows the request untouched in the common
/// case where nothing needs loading; only requests with policy-matching
/// local-file parts pay for a copy.
pub(crate) async fn resolve(request: &Request, policy: AttachmentPolicy) -> Cow<'_, Request> {
    if !needs_resolution(request, policy) {
        return Cow::Borrowed(request);
    }

    let mut resolved = request.clone();
    for message in &mut resolved.messages {
        let mut new_content = Vec::with_capacity(message.content.len());
        for part in std::mem::take(&mut message.content) {
            if let Some(part) = resolve_part(part, policy).await {
                new_content.push(part);
            }
        }
        message.content = new_content;
    }
    Cow::Owned(resolved)
}

/// Whether any part is a policy-matching local-file attachment.
fn needs_resolution(request: &Request, policy: AttachmentPolicy) -> bool {
    request
        .messages
        .iter()
        .flat_map(|message| &message.content)
        .any(|part| match part {
            ContentPart::Image(img) => policy.images && is_local_file(img.url.as_deref()),
            ContentPart::Document(doc) => policy.documents && is_local_file(doc.url.as_deref()),
            ContentPart::Audio(audio) => policy.audio && is_local_file(audio.url.as_deref()),
            _ => false,
        })
}

/// Resolve a single part. `None` means the part was dropped (load error).
async fn resolve_part(part: ContentPart, policy: AttachmentPolicy) -> Option<ContentPart> {
    match part {
        ContentPart::Image(img) if policy.images && is_local_file(img.url.as_deref()) => {
            // `is_local_file` guarantees `url` is `Some`.
            let url = img.url.as_deref().unwrap_or_default();
            match common::load_file_bytes(url).await {
                Ok((data, mime)) => Some(ContentPart::Image(ImageData {
                    url:        None,
                    data:       Some(data),
                    media_type: Some(mime),
                    detail:     img.detail,
                })),
                Err(_) => None,
            }
        }
        ContentPart::Document(doc) if policy.documents && is_local_file(doc.url.as_deref()) => {
            let url = doc.url.as_deref().unwrap_or_default();
            match common::load_file_bytes(url).await {
                Ok((data, mime)) => Some(ContentPart::Document(DocumentData {
                    url:        None,
                    data:       Some(data),
                    media_type: Some(mime),
                    file_name:  doc.file_name,
                })),
                Err(_) => None,
            }
        }
        ContentPart::Audio(audio) if policy.audio && is_local_file(audio.url.as_deref()) => {
            let url = audio.url.as_deref().unwrap_or_default();
            match common::load_file_bytes(url).await {
                Ok((data, mime)) => Some(ContentPart::Audio(AudioData {
                    url:        None,
                    data:       Some(data),
                    media_type: Some(mime),
                })),
                Err(_) => None,
            }
        }
        other => Some(other),
    }
}

fn is_local_file(url: Option<&str>) -> bool {
    url.is_some_and(common::is_file_path)
}
