pub(crate) fn normalize_sid(sid: &str) -> Option<&str> {
    if !sid.starts_with("S-1-") {
        return None;
    }

    if sid
        .chars()
        .any(|c| c.is_whitespace() || matches!(c, '(' | ')' | ';'))
    {
        return None;
    }

    if !sid
        .chars()
        .all(|c| c.is_ascii_digit() || matches!(c, 'S' | '-'))
    {
        return None;
    }

    let mut parts = sid.split('-');
    if parts.next() != Some("S") || parts.next() != Some("1") {
        return None;
    }
    if !parts.all(|part| !part.is_empty() && part.chars().all(|c| c.is_ascii_digit())) {
        return None;
    }

    Some(sid)
}

#[cfg(test)]
mod tests {
    use super::normalize_sid;

    #[test]
    fn sid_validation_accepts_normal_sid() {
        assert_eq!(
            normalize_sid("S-1-5-21-1-2-3-1001"),
            Some("S-1-5-21-1-2-3-1001")
        );
    }

    #[test]
    fn sid_validation_rejects_sddl_breakout_characters() {
        assert_eq!(normalize_sid("S-1-5-21-1-2-3-1001)"), None);
        assert_eq!(normalize_sid("S-1-5-21-1-2-3-1001;"), None);
        assert_eq!(normalize_sid("S-1-5-21-1-2-3-1001("), None);
        assert_eq!(normalize_sid("S-1-5-21-1-2-3-1001 "), None);
    }

    #[test]
    fn sid_validation_rejects_malformed_sid() {
        assert_eq!(normalize_sid(""), None);
        assert_eq!(normalize_sid("S-2-5-21-1-2-3-1001"), None);
        assert_eq!(normalize_sid("S-1-5-21-1-2-3-"), None);
        assert_eq!(normalize_sid("S-1-5-21-1-2-3-user"), None);
    }
}
