//! User-id derivation and token generation.

/// "Jordan Boswell" → "user_jordan-boswell". Duplicate names surface as
/// operator_user_exists from the store — rename, don't silently suffix.
pub fn user_id_from_display_name(display_name: &str) -> Result<String, &'static str> {
    let slug: String = display_name
        .trim()
        .to_ascii_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect::<String>()
        .split('-')
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("-");
    if slug.is_empty() {
        return Err("operator_user_name_required");
    }
    Ok(format!("user_{}", &slug[..slug.len().min(48)]))
}

/// 256-bit personal bearer token from the OS CSPRNG ("bosu_" + 64 hex chars).
/// No rand dependency: /dev/urandom is the Linux deployment posture.
pub fn generate_token() -> Result<String, std::io::Error> {
    use std::io::Read;
    let mut bytes = [0u8; 32];
    std::fs::File::open("/dev/urandom")?.read_exact(&mut bytes)?;
    let mut token = String::with_capacity(5 + 64);
    token.push_str("bosu_");
    for byte in bytes {
        token.push_str(&format!("{byte:02x}"));
    }
    Ok(token)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_id_slugifies_display_names() {
        assert_eq!(
            user_id_from_display_name("Jordan Boswell").unwrap(),
            "user_jordan-boswell"
        );
        assert_eq!(
            user_id_from_display_name("  Casey  ").unwrap(),
            "user_casey"
        );
        assert!(user_id_from_display_name("  !!  ").is_err());
    }

    #[test]
    fn tokens_are_unique_and_well_formed() {
        let a = generate_token().expect("token");
        let b = generate_token().expect("token");
        assert_ne!(a, b);
        assert!(a.starts_with("bosu_"));
        assert_eq!(a.len(), 5 + 64);
    }
}
