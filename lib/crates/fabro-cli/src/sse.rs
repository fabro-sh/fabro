pub(crate) fn drain_sse_payloads(buffer: &mut Vec<u8>, finalize: bool) -> Vec<String> {
    let mut payloads = Vec::new();

    while let Some(pos) = buffer.iter().position(|byte| *byte == b'\n') {
        let line = buffer.drain(..=pos).collect::<Vec<_>>();
        if let Some(payload) = sse_data_line(&line) {
            payloads.push(payload);
        }
    }

    if finalize && !buffer.is_empty() {
        let line = std::mem::take(buffer);
        if let Some(payload) = sse_data_line(&line) {
            payloads.push(payload);
        }
    }

    payloads
}

fn sse_data_line(line: &[u8]) -> Option<String> {
    let line = String::from_utf8_lossy(line);
    let line = line.trim_end_matches(['\r', '\n']);
    line.strip_prefix("data:")
        .map(|data| data.trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::drain_sse_payloads;

    #[test]
    fn drain_sse_payloads_handles_chunk_boundaries() {
        let mut buffer = b"data: {\"a\":1}\n\ndat".to_vec();

        assert_eq!(drain_sse_payloads(&mut buffer, false), vec![r#"{"a":1}"#]);
        assert_eq!(buffer, b"dat");

        buffer.extend_from_slice(b"a: {\"b\":2}\n\n");
        assert_eq!(drain_sse_payloads(&mut buffer, false), vec![r#"{"b":2}"#]);
        assert!(buffer.is_empty());
    }

    #[test]
    fn drain_sse_payloads_ignores_keepalives_and_finalizes_trailing_line() {
        let mut buffer = b": keep-alive\n\n\ndata: {\"a\":1}".to_vec();

        assert_eq!(drain_sse_payloads(&mut buffer, true), vec![r#"{"a":1}"#]);
        assert!(buffer.is_empty());
    }
}
