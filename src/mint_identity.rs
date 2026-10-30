//! Canonical mint identity v1. See docs/mint-identity.md.
#[cfg(test)]
mod tests {
    use super::canonical;
    #[test]
    fn contract() {
        for (a, b) in [
            ("HTTP://MINT.example:80/", "http://mint.example"),
            (
                "https://MINT.example:443/mint/",
                "https://mint.example/mint",
            ),
            (
                "https://mint.example:8443/Mint",
                "https://mint.example:8443/Mint",
            ),
        ] {
            assert_eq!(canonical(a).unwrap(), b);
            assert_eq!(canonical(b).unwrap(), b);
        }
        for bad in [
            "mint",
            "https:mint.example",
            "https:///mint.example",
            "ftp://mint.example",
            "https://u:p@mint.example",
            "https://@mint.example",
            "https://mint.example/#x",
            "https://mint.example?x",
            "https://mint.example/a/../b",
            "https://mint.example/%2e",
            "https://mint.example/a//b",
            "https://mint.example:",
            "https://mint.example./",
            "https://127.1",
            "https://mint.example/ x",
            "https://mint.example\\x",
        ] {
            assert!(canonical(bad).is_err(), "{bad}");
        }
        assert_ne!(
            canonical("https://mint.example/a").unwrap(),
            canonical("https://mint.example/b").unwrap()
        );
    }
}
pub(crate) fn canonical(raw: &str) -> Result<String, ()> {
    // Reject URL-parser repair/aliasing rather than guessing server semantics.
    if !raw.is_ascii()
        || raw
            .bytes()
            .any(|b| b.is_ascii_whitespace() || b.is_ascii_control())
        || raw.contains(['\\', '%', '?', '#', '@'])
    {
        return Err(());
    }
    let (scheme, rest) = raw.split_once("://").ok_or(())?;
    if !scheme.eq_ignore_ascii_case("http") && !scheme.eq_ignore_ascii_case("https") {
        return Err(());
    }
    let authority = rest.split('/').next().ok_or(())?;
    if authority.is_empty() || authority.ends_with(':') || authority.ends_with('.') {
        return Err(());
    }
    let path = &rest[authority.len()..];
    if path.contains("//") || path.split('/').any(|s| s == "." || s == "..") {
        return Err(());
    }
    let url = url::Url::parse(raw).map_err(|_| ())?;
    if url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(());
    }
    // Reject nonstandard IPv4 spellings and DNS trailing dots (including with port).
    let host = url.host_str().ok_or(())?;
    if host.ends_with('.') {
        return Err(());
    }
    let input_host = if authority.starts_with('[') {
        authority.split_once(']').ok_or(())?.0.to_owned() + "]"
    } else {
        authority.split(':').next().ok_or(())?.to_ascii_lowercase()
    };
    if input_host != host {
        return Err(());
    }
    // One optional trailing slash is irrelevant; internal path and port are preserved.
    Ok(url.as_str().trim_end_matches('/').to_owned())
}
