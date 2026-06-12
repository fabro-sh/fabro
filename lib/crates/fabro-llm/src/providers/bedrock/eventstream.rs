//! Decoder for Bedrock's `application/vnd.amazon.eventstream` streaming
//! responses.
//!
//! ConverseStream wraps each event in a binary event-stream frame: the event
//! name (`messageStart`, `contentBlockDelta`, `metadata`, ...) travels in the
//! frame's `:event-type` header and the payload is that event's JSON
//! directly. (The base64 `{"bytes": ...}` wrapping belongs to
//! `InvokeModelWithResponseStream`'s `PayloadPart` and does not apply here.)
//! Exception and error frames are surfaced as stream errors.

use aws_smithy_eventstream::frame::{DecodedFrame, MessageFrameDecoder};
use aws_smithy_types::event_stream::Message;
use aws_smithy_types::str_bytes::StrBytes;
use bytes::BytesMut;

use crate::error::Error;

/// One decoded ConverseStream event: the `:event-type` header value plus the
/// frame's JSON payload, ready to feed a stream decoder.
#[derive(Debug)]
pub(crate) struct DecodedEvent {
    pub event_type: String,
    pub payload:    String,
}

/// Incremental decoder over event-stream bytes.
pub(crate) struct FrameDecoder {
    inner:  MessageFrameDecoder,
    buffer: BytesMut,
}

impl FrameDecoder {
    pub(crate) fn new() -> Self {
        Self {
            inner:  MessageFrameDecoder::new(),
            buffer: BytesMut::new(),
        }
    }

    /// Feed newly received bytes and return any complete events decoded from
    /// them. Bedrock exception and error frames are surfaced as errors.
    pub(crate) fn push(&mut self, bytes: &[u8]) -> Result<Vec<DecodedEvent>, Error> {
        self.buffer.extend_from_slice(bytes);
        let mut events = Vec::new();
        loop {
            // `decode_frame` advances `self.buffer` and retains partial-frame
            // state internally, so repeated calls over a growing buffer work.
            let frame = self.inner.decode_frame(&mut self.buffer).map_err(|e| {
                Error::stream_error(
                    format!("bedrock event-stream decode: {e}"),
                    std::io::Error::other(e.to_string()),
                )
            })?;
            match frame {
                DecodedFrame::Complete(message) => {
                    if let Some(event) = Self::message_to_event(&message)? {
                        events.push(event);
                    }
                }
                DecodedFrame::Incomplete => break,
            }
        }
        Ok(events)
    }

    /// Classify one event-stream message.
    ///
    /// `event` frames yield their `:event-type` name and JSON payload;
    /// `exception` frames (modeled AWS errors such as `throttlingException`,
    /// arriving in-band after HTTP 200) and `error` frames (unmodeled) are
    /// turned into errors. Frames without an event type are skipped.
    fn message_to_event(message: &Message) -> Result<Option<DecodedEvent>, Error> {
        match header_str(message, ":message-type") {
            Some("exception") => {
                let kind = header_str(message, ":exception-type").unwrap_or("unknown");
                let body = String::from_utf8_lossy(message.payload());
                Err(Error::stream_error(
                    format!("bedrock stream exception ({kind}): {body}"),
                    std::io::Error::other("bedrock event-stream exception frame"),
                ))
            }
            Some("error") => {
                let code = header_str(message, ":error-code").unwrap_or("unknown");
                let detail = header_str(message, ":error-message").unwrap_or("");
                Err(Error::stream_error(
                    format!("bedrock stream error ({code}): {detail}"),
                    std::io::Error::other("bedrock event-stream error frame"),
                ))
            }
            _ => {
                let Some(event_type) = header_str(message, ":event-type") else {
                    return Ok(None);
                };
                Ok(Some(DecodedEvent {
                    event_type: event_type.to_string(),
                    payload:    String::from_utf8_lossy(message.payload()).into_owned(),
                }))
            }
        }
    }
}

/// Read a string-valued event-stream header by name.
fn header_str<'a>(message: &'a Message, name: &str) -> Option<&'a str> {
    message
        .headers()
        .iter()
        .find(|header| header.name().as_str() == name)
        .and_then(|header| header.value().as_string().ok())
        .map(StrBytes::as_str)
}

#[cfg(test)]
pub(crate) mod tests {
    use aws_smithy_eventstream::frame::write_message_to;
    use aws_smithy_types::event_stream::{Header, HeaderValue, Message};

    use super::*;

    /// Build one ConverseStream event frame: event name in `:event-type`,
    /// payload = the event JSON directly.
    fn encode_event_frame(event_type: &str, payload_json: &str) -> Vec<u8> {
        let message = Message::new(payload_json.as_bytes().to_vec())
            .add_header(Header::new(
                ":message-type",
                HeaderValue::String("event".into()),
            ))
            .add_header(Header::new(
                ":event-type",
                HeaderValue::String(event_type.to_string().into()),
            ))
            .add_header(Header::new(
                ":content-type",
                HeaderValue::String("application/json".into()),
            ));
        let mut buf = Vec::new();
        write_message_to(&message, &mut buf).unwrap();
        buf
    }

    /// Build a full streaming body from `(event_type, payload_json)` pairs.
    pub(crate) fn build_stream_body(events: &[(&str, &str)]) -> Vec<u8> {
        let mut body = Vec::new();
        for (event_type, payload) in events {
            body.extend_from_slice(&encode_event_frame(event_type, payload));
        }
        body
    }

    #[test]
    fn decodes_event_frame_to_typed_payload() {
        let frame = encode_event_frame(
            "contentBlockDelta",
            r#"{"delta":{"text":"hi"},"contentBlockIndex":0}"#,
        );
        let mut decoder = FrameDecoder::new();
        let events = decoder.push(&frame).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_type, "contentBlockDelta");
        let payload: serde_json::Value = serde_json::from_str(&events[0].payload).unwrap();
        assert_eq!(payload["delta"]["text"], "hi");
    }

    #[test]
    fn reassembles_frame_split_across_pushes() {
        let frame = encode_event_frame("messageStop", r#"{"stopReason":"end_turn"}"#);
        let split = frame.len() / 2;
        let mut decoder = FrameDecoder::new();
        assert!(decoder.push(&frame[..split]).unwrap().is_empty());
        let events = decoder.push(&frame[split..]).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_type, "messageStop");
    }

    #[test]
    fn exception_frame_surfaces_as_error() {
        let message = Message::new(br#"{"message":"Too many requests"}"#.to_vec())
            .add_header(Header::new(
                ":message-type",
                HeaderValue::String("exception".into()),
            ))
            .add_header(Header::new(
                ":exception-type",
                HeaderValue::String("throttlingException".into()),
            ));
        let mut buf = Vec::new();
        write_message_to(&message, &mut buf).unwrap();

        let mut decoder = FrameDecoder::new();
        let err = decoder.push(&buf).unwrap_err();
        let rendered = err.to_string();
        assert!(rendered.contains("throttlingException"), "{rendered}");
        assert!(rendered.contains("Too many requests"), "{rendered}");
    }

    #[test]
    fn unmodeled_error_frame_surfaces_as_error() {
        let message = Message::new(Vec::new())
            .add_header(Header::new(
                ":message-type",
                HeaderValue::String("error".into()),
            ))
            .add_header(Header::new(
                ":error-code",
                HeaderValue::String("InternalError".into()),
            ))
            .add_header(Header::new(
                ":error-message",
                HeaderValue::String("stream broke".into()),
            ));
        let mut buf = Vec::new();
        write_message_to(&message, &mut buf).unwrap();

        let mut decoder = FrameDecoder::new();
        let err = decoder.push(&buf).unwrap_err();
        let rendered = err.to_string();
        assert!(rendered.contains("InternalError"), "{rendered}");
    }
}
