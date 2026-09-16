//! Token-id / secret generation and capability parsing.

use crate::http::OperatorCapability;

/// 256-bit machine bearer from the OS CSPRNG ("bost_" + 64 hex chars).
pub fn generate_secret() -> Result<String, std::io::Error> {
    let bytes = random_bytes(32)?;
    Ok(format!("bost_{}", hex(&bytes)))
}

/// Short public id ("apitok_" + 16 hex chars).
pub fn generate_token_id() -> Result<String, std::io::Error> {
    let bytes = random_bytes(8)?;
    Ok(format!("apitok_{}", hex(&bytes)))
}

pub fn parse_label(label: &str) -> Result<String, &'static str> {
    let label = label.trim();
    if label.is_empty() {
        return Err("operator_api_token_label_required");
    }
    if label.len() > 80 {
        return Err("operator_api_token_label_too_long");
    }
    Ok(label.to_string())
}

pub fn parse_capabilities(raw: &[String]) -> Result<Vec<OperatorCapability>, &'static str> {
    if raw.is_empty() {
        return Err("operator_capabilities_required");
    }
    let mut parsed = Vec::new();
    for value in raw {
        let cap = OperatorCapability::parse(value.trim()).ok_or("operator_capability_unknown")?;
        if !parsed.contains(&cap) {
            parsed.push(cap);
        }
    }
    parsed.sort();
    Ok(parsed)
}

fn random_bytes(len: usize) -> Result<Vec<u8>, std::io::Error> {
    use std::io::Read;
    let mut bytes = vec![0u8; len];
    std::fs::File::open("/dev/urandom")?.read_exact(&mut bytes)?;
    Ok(bytes)
}

fn hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_capabilities_allowlists_and_rejects_empty_or_unknown() {
        assert_eq!(
            parse_capabilities(&["social_publishing:read".into()]).unwrap(),
            vec![OperatorCapability::SocialPublishingRead]
        );
        assert_eq!(
            parse_capabilities(&[
                "agent_mcp:ingest".into(),
                "social_publishing:approve".into(),
                "social_publishing:approve".into(),
            ])
            .unwrap(),
            vec![
                OperatorCapability::SocialPublishingApprove,
                OperatorCapability::AgentMcpIngest,
            ]
        );
        assert_eq!(
            parse_capabilities(&[]).unwrap_err(),
            "operator_capabilities_required"
        );
        assert_eq!(
            parse_capabilities(&["accounting:read".into()]).unwrap_err(),
            "operator_capability_unknown"
        );
    }

    #[test]
    fn parse_label_trims_and_rejects_blank() {
        assert_eq!(parse_label("  Slack bridge  ").unwrap(), "Slack bridge");
        assert_eq!(
            parse_label("   ").unwrap_err(),
            "operator_api_token_label_required"
        );
        assert_eq!(
            parse_label(&"x".repeat(81)).unwrap_err(),
            "operator_api_token_label_too_long"
        );
    }
}
