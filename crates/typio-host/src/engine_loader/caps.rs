//! Capability negotiation between the host and engine manifests.
//!
//! The host advertises a static set of capability names it can fulfil for an
//! engine. An engine's `required` list must be a subset of this set, otherwise
//! the manifest is rejected. Missing `optional` capabilities are silently
//! downgraded — the engine loads but cannot use them.

use std::collections::HashSet;

/// Capabilities the host can offer to engines.
///
/// Default is the standard keyboard-IM capability set. Voice support adds
/// `voice_input` and `continuous_voice`.
#[derive(Debug, Clone)]
pub struct HostCapabilities {
    caps: HashSet<String>,
}

impl Default for HostCapabilities {
    fn default() -> Self {
        let caps = [
            "preedit",
            "candidates",
            "prediction",
            "punctuation",
            "learning",
        ]
        .iter()
        .map(|s| (*s).to_string())
        .collect();
        Self { caps }
    }
}

impl HostCapabilities {
    /// Construct an empty capability set (host supports nothing).
    pub fn empty() -> Self {
        Self {
            caps: HashSet::new(),
        }
    }

    /// Add a capability the host provides.
    pub fn with(mut self, cap: &str) -> Self {
        self.caps.insert(cap.to_string());
        self
    }

    /// Add the voice capability set (`voice_input`, `continuous_voice`).
    pub fn with_voice(mut self) -> Self {
        self.caps.insert("voice_input".to_string());
        self.caps.insert("continuous_voice".to_string());
        self
    }

    /// True iff the host provides the named capability.
    pub fn supports(&self, name: &str) -> bool {
        self.caps.contains(name)
    }

    /// Iterate over all advertised capability names.
    pub fn iter(&self) -> impl Iterator<Item = &str> {
        self.caps.iter().map(|s| s.as_str())
    }

    /// Negotiate the host's capabilities against an engine's declaration.
    ///
    /// Returns `Ok(missing_optional)` if every required capability is
    /// provided by the host. `missing_optional` lists the optional
    /// capabilities the host does NOT provide (so the caller can log them
    /// at info level — the engine still loads). Returns
    /// `Err(missing_required)` if any required capability is missing; the
    /// caller must refuse to load the manifest in that case.
    pub fn negotiate(
        &self,
        required: &[String],
        optional: &[String],
    ) -> Result<Vec<String>, NegotiationFailure> {
        let missing_required: Vec<String> = required
            .iter()
            .filter(|c| !self.supports(c))
            .cloned()
            .collect();
        if !missing_required.is_empty() {
            return Err(NegotiationFailure {
                missing_required,
                missing_optional: vec![],
            });
        }
        let missing_optional: Vec<String> = optional
            .iter()
            .filter(|c| !self.supports(c))
            .cloned()
            .collect();
        Ok(missing_optional)
    }
}

/// Failure result of [`HostCapabilities::negotiate`]: at least one required
/// capability is not provided by the host. The manifest must be refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NegotiationFailure {
    /// Required-capability names the host does not provide.
    pub missing_required: Vec<String>,
    /// Always empty in this variant (the negotiation short-circuits on the
    /// first missing required cap); included only so the failure type is
    /// self-describing.
    pub missing_optional: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_includes_keyboard_im_set() {
        let caps = HostCapabilities::default();
        assert!(caps.supports("preedit"));
        assert!(caps.supports("candidates"));
        assert!(caps.supports("prediction"));
        assert!(caps.supports("punctuation"));
        assert!(caps.supports("learning"));
    }

    #[test]
    fn default_excludes_voice() {
        let caps = HostCapabilities::default();
        assert!(!caps.supports("voice_input"));
        assert!(!caps.supports("continuous_voice"));
    }

    #[test]
    fn with_voice_adds_voice_capabilities() {
        let caps = HostCapabilities::default().with_voice();
        assert!(caps.supports("voice_input"));
        assert!(caps.supports("continuous_voice"));
    }

    #[test]
    fn negotiate_ok_when_required_subset_of_host() {
        let caps = HostCapabilities::default();
        let required = vec!["preedit".to_string(), "candidates".to_string()];
        let optional = vec!["prediction".to_string()];
        let missing_optional = caps.negotiate(&required, &optional).unwrap();
        assert!(missing_optional.is_empty()); // host provides all of optional too
    }

    #[test]
    fn negotiate_returns_missing_optional_list() {
        let caps = HostCapabilities::default();
        let optional = vec!["prediction".to_string(), "ai_suggest".to_string()];
        let missing = caps.negotiate(&[], &optional).unwrap();
        assert_eq!(missing, vec!["ai_suggest".to_string()]);
    }

    #[test]
    fn negotiate_fails_when_required_not_subset() {
        let caps = HostCapabilities::default();
        let required = vec!["preedit".to_string(), "ai_suggest".to_string()];
        let err = caps.negotiate(&required, &[]).unwrap_err();
        assert_eq!(err.missing_required, vec!["ai_suggest".to_string()]);
    }
}
