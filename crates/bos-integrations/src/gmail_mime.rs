//! Pure RFC2822 message construction + Gmail web-safe base64url encoding.
//! No I/O. CR/LF in header inputs is stripped (header-injection guard).

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};

/// Neutralize CR/LF so header values can't inject extra headers.
/// Truncates at the first CR or LF — anything after could be a folded/injected header.
fn sanitize_header(value: &str) -> String {
    let truncated = value
        .find(['\r', '\n'])
        .map(|i| &value[..i])
        .unwrap_or(value);
    truncated.trim().to_string()
}

/// Build a minimal plain-text RFC2822 message and return it as Gmail-ready
/// base64url (unpadded, url-safe — Gmail `raw` format).
/// When `from_email` is omitted, Gmail fills the authenticated user's default sender.
pub fn build_raw_message(
    from_email: Option<&str>,
    to: &[String],
    cc: &[String],
    subject: &str,
    body: &str,
) -> String {
    build_raw_message_with_reply_headers(from_email, to, cc, subject, body, None, &[])
}

/// Build a minimal plain-text RFC2822 message with optional reply headers and
/// return it as Gmail-ready base64url.
pub fn build_raw_message_with_reply_headers(
    from_email: Option<&str>,
    to: &[String],
    cc: &[String],
    subject: &str,
    body: &str,
    reply_message_id: Option<&str>,
    reference_message_ids: &[String],
) -> String {
    let from_hdr = from_email
        .map(sanitize_header)
        .filter(|value| !value.is_empty())
        .map(|value| format!("From: {value}\r\n"))
        .unwrap_or_default();
    let to_hdr = to
        .iter()
        .map(|t| sanitize_header(t))
        .filter(|t| !t.is_empty())
        .collect::<Vec<_>>()
        .join(", ");
    let cc_hdr = cc
        .iter()
        .map(|t| sanitize_header(t))
        .filter(|t| !t.is_empty())
        .collect::<Vec<_>>()
        .join(", ");
    let cc_hdr = if cc_hdr.is_empty() {
        String::new()
    } else {
        format!("Cc: {cc_hdr}\r\n")
    };
    let subject = sanitize_header(subject);
    let reply_hdr = reply_message_id
        .map(sanitize_header)
        .filter(|value| valid_message_id(value))
        .map(|value| format!("In-Reply-To: {value}\r\n"))
        .unwrap_or_default();
    let references = reference_message_ids
        .iter()
        .map(|id| sanitize_header(id))
        .filter(|id| valid_message_id(id))
        .collect::<Vec<_>>()
        .join(" ");
    let references_hdr = if references.is_empty() {
        String::new()
    } else {
        format!("References: {references}\r\n")
    };
    let message = format!(
        "{from_hdr}To: {to_hdr}\r\n{cc_hdr}Subject: {subject}\r\n{reply_hdr}{references_hdr}MIME-Version: 1.0\r\nContent-Type: text/plain; charset=\"UTF-8\"\r\n\r\n{body}"
    );
    URL_SAFE_NO_PAD.encode(message.as_bytes())
}

fn valid_message_id(value: &str) -> bool {
    let inner = value
        .strip_prefix('<')
        .and_then(|value| value.strip_suffix('>'))
        .unwrap_or_default();
    value.len() >= 3
        && value.len() <= 255
        && inner.contains('@')
        && !inner
            .chars()
            .any(|ch| ch.is_control() || ch.is_whitespace() || ch == '<' || ch == '>')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decode_b64url(s: &str) -> Vec<u8> {
        URL_SAFE_NO_PAD
            .decode(s.as_bytes())
            .expect("valid base64url")
    }

    #[test]
    fn builds_rfc2822_and_roundtrips() {
        let raw = build_raw_message(
            None,
            &["jordan@example.test".to_string()],
            &[],
            "Re: order",
            "Hello there",
        );
        assert!(!raw.contains('='), "base64url must be unpadded");
        assert!(
            !raw.contains('+') && !raw.contains('/'),
            "must be url-safe alphabet"
        );
        let decoded = String::from_utf8(decode_b64url(&raw)).unwrap();
        assert!(decoded.contains("To: jordan@example.test"));
        assert!(decoded.contains("Subject: Re: order"));
        assert!(decoded.contains("\r\n\r\nHello there"));
    }

    #[test]
    fn strips_header_injection() {
        let raw = build_raw_message(
            Some("Alias <alias@example.test>\r\nBcc: evil@business-a24088ba7a.test"),
            &["a@business-ae869e1a19.test\r\nBcc: evil@business-a24088ba7a.test".to_string()],
            &["c@business-f692450f01.test\r\nBcc: evil@business-a24088ba7a.test".to_string()],
            "hi\r\nX-Injected: 1",
            "body",
        );
        let decoded = String::from_utf8(decode_b64url(&raw)).unwrap();
        assert!(!decoded.contains("Bcc:"), "CRLF in To must be neutralized");
        assert!(
            !decoded.contains("X-Injected"),
            "CRLF in Subject must be neutralized"
        );
        assert!(
            !decoded.contains("Bcc:"),
            "CRLF in From must be neutralized"
        );
    }

    #[test]
    fn joins_multiple_recipients() {
        let raw = build_raw_message(
            None,
            &[
                "a@business-ae869e1a19.test".to_string(),
                "c@business-f692450f01.test".to_string(),
            ],
            &[],
            "s",
            "b",
        );
        let decoded = String::from_utf8(decode_b64url(&raw)).unwrap();
        assert!(decoded.contains("To: a@business-ae869e1a19.test, c@business-f692450f01.test"));
    }

    #[test]
    fn includes_explicit_from_when_provided() {
        let raw = build_raw_message(
            Some("Avery <user@example.test>"),
            &["customer@example.test".to_string()],
            &[],
            "Re: request",
            "body",
        );
        let decoded = String::from_utf8(decode_b64url(&raw)).unwrap();
        assert!(decoded.starts_with("From: Avery <user@example.test>\r\n"));
        assert!(decoded.contains("To: customer@example.test"));
    }

    #[test]
    fn includes_cc_when_present() {
        let raw = build_raw_message(
            None,
            &["customer@example.test".to_string()],
            &[
                "ops@example.test".to_string(),
                "team@example.test".to_string(),
            ],
            "Re: request",
            "body",
        );
        let decoded = String::from_utf8(decode_b64url(&raw)).unwrap();
        assert!(decoded.contains("Cc: ops@example.test, team@example.test\r\n"));
    }

    #[test]
    fn includes_reply_headers_when_present() {
        let raw = build_raw_message_with_reply_headers(
            None,
            &["customer@example.test".to_string()],
            &[],
            "Re: request",
            "body",
            Some("<source@example.test>"),
            &[
                "<root@example.test>".to_string(),
                "<source@example.test>".to_string(),
            ],
        );
        let decoded = String::from_utf8(decode_b64url(&raw)).unwrap();
        assert!(decoded.contains("In-Reply-To: <source@example.test>\r\n"));
        assert!(decoded.contains("References: <root@example.test> <source@example.test>\r\n"));
    }

    #[test]
    fn drops_invalid_reply_header_message_ids() {
        let raw = build_raw_message_with_reply_headers(
            None,
            &["customer@example.test".to_string()],
            &[],
            "Re: request",
            "body",
            Some("<bad id@example.test>"),
            &[
                "<root@example.test>".to_string(),
                "<bad\r\nX-Injected: yes@example.test>".to_string(),
                "<nested<bad>@example.test>".to_string(),
            ],
        );
        let decoded = String::from_utf8(decode_b64url(&raw)).unwrap();
        assert!(!decoded.contains("In-Reply-To:"));
        assert!(decoded.contains("References: <root@example.test>\r\n"));
        assert!(!decoded.contains("X-Injected"));
    }
}
