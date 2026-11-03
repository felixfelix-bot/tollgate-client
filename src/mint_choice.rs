//! The mint the gate itself advertises, vs. a mint the user configured.
//!
//! The gate's advertisement is the authority on which mints it accepts: its
//! `price_per_step` tag names a mint URL per offer, and a token minted anywhere
//! else is refused. So the phone must not carry a hardcoded mint default — it
//! has to take the mint from the advert it just parsed, or from an explicit
//! user override, and never invent one.
//!
//! This module holds that choice as a pure function so the rule is testable
//! without a device, a gate, or a network.

/// Which mint to ask the gate for a quote at.
///
/// `override_url` is what the user configured (empty = unset). `advertised` is
/// every mint the gate currently advertises, in advert order.
///
/// Rules, in order:
/// 1. A non-empty override wins **only if the gate advertises it**. Asking a
///    gate for a quote at a mint it does not accept is a guaranteed refusal, so
///    an override the gate does not honour is not a preference, it is an error.
/// 2. With no override, the first advertised mint is used.
/// 3. No override and no advertised mint is an error — never a guess.
pub fn choose_mint(override_url: &str, advertised: &[String]) -> Result<String, String> {
    let want = override_url.trim();
    if want.is_empty() {
        return match advertised.first() {
            Some(m) if !m.trim().is_empty() => Ok(m.trim().to_string()),
            _ => Err("gateway advertises no mint this client can pay".to_string()),
        };
    }
    if advertised.iter().any(|m| same_mint(m, want)) {
        // Return the advertised spelling, not the user's: the gate compares
        // mint identity itself, and echoing its own string removes one way for
        // the two sides to disagree about the same mint.
        return Ok(advertised
            .iter()
            .find(|m| same_mint(m, want))
            .map(|m| m.trim().to_string())
            .unwrap_or_else(|| want.to_string()));
    }
    Err(format!(
        "gateway does not accept the configured mint {want}; it advertises: {}",
        advertised.join(", ")
    ))
}

/// Two mint URLs are the same mint if they differ only by a trailing slash or
/// by case in the scheme/host.
///
/// A mint's identity is the URL, and the module canonicalises through its own
/// `NormalizeMintURL`; this is the client's matching subset. It is deliberately
/// conservative — it does not follow redirects, strip default ports, or
/// normalise paths, because two different paths on one host are two mints.
fn same_mint(a: &str, b: &str) -> bool {
    fn norm(s: &str) -> String {
        let t = s.trim().trim_end_matches('/');
        // Split authority from path so only scheme+host are case-folded.
        match t.find("://") {
            Some(i) => {
                let (scheme, rest) = t.split_at(i);
                // `rest` begins with "://" (split_at cuts at the colon), so strip
                // the whole delimiter before looking for the path. Slicing a
                // fixed 2 chars instead leaves a leading `/`, and that slash is
                // then mistaken for the authority delimiter — the host never
                // gets case-folded and `HTTPS://MINT.ONE` stops matching.
                let after = rest.trim_start_matches("://");
                let (authority, path) = match after.find('/') {
                    Some(j) => (&after[..j], &after[j..]),
                    None => (after, ""),
                };
                format!(
                    "{}://{}{}",
                    scheme.to_ascii_lowercase(),
                    authority.to_ascii_lowercase(),
                    path
                )
            }
            None => t.to_ascii_lowercase(),
        }
    }
    norm(a) == norm(b)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn adv() -> Vec<String> {
        vec![
            "https://mint.one/".to_string(),
            "https://mint.two/".to_string(),
        ]
    }

    #[test]
    fn with_no_override_the_gates_own_mint_is_used() {
        assert_eq!(choose_mint("", &adv()).unwrap(), "https://mint.one/");
    }

    #[test]
    fn an_override_the_gate_accepts_is_honoured_and_returned_as_advertised() {
        // User typed no trailing slash; the advert has one. The gate's spelling
        // is what comes back, so the two sides agree on one string.
        assert_eq!(
            choose_mint("https://mint.two", &adv()).unwrap(),
            "https://mint.two/"
        );
    }

    #[test]
    fn an_override_the_gate_does_not_accept_is_an_error_naming_both_sides() {
        let err = choose_mint("https://mint.three/", &adv()).unwrap_err();
        assert!(err.contains("mint.three"), "{err}");
        assert!(err.contains("mint.one"), "{err}");
    }

    #[test]
    fn no_override_and_no_advertised_mint_is_an_error_not_a_guess() {
        let err = choose_mint("", &[]).unwrap_err();
        assert!(err.contains("no mint"), "{err}");
    }

    #[test]
    fn an_override_survives_a_scheme_case_difference() {
        assert_eq!(
            choose_mint("HTTPS://MINT.ONE/", &adv()).unwrap(),
            "https://mint.one/"
        );
    }

    #[test]
    fn two_paths_on_one_host_are_not_the_same_mint() {
        let only = vec!["https://mint.one/cashu".to_string()];
        assert!(choose_mint("https://mint.one/other", &only).is_err());
    }

    #[test]
    fn a_blank_advertised_entry_is_never_chosen() {
        let blanks = vec!["".to_string(), "  ".to_string()];
        assert!(choose_mint("", &blanks).is_err());
    }
}
