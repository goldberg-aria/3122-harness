use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApprovalPolicy {
    Prompt,
    Auto,
}

impl ApprovalPolicy {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Prompt => "prompt",
            Self::Auto => "auto",
        }
    }

    pub fn parse(input: &str) -> Option<Self> {
        match input.trim() {
            "prompt" => Some(Self::Prompt),
            "auto" => Some(Self::Auto),
            _ => None,
        }
    }
}

impl fmt::Display for ApprovalPolicy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VerificationPolicy {
    Off,
    Annotate,
    Require,
}

impl VerificationPolicy {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Annotate => "annotate",
            Self::Require => "require",
        }
    }

    pub fn parse(input: &str) -> Option<Self> {
        match input.trim() {
            "off" => Some(Self::Off),
            "annotate" => Some(Self::Annotate),
            "require" => Some(Self::Require),
            _ => None,
        }
    }
}

impl fmt::Display for VerificationPolicy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::{ApprovalPolicy, VerificationPolicy};

    #[test]
    fn parses_known_approval_policies() {
        assert_eq!(
            ApprovalPolicy::parse("prompt"),
            Some(ApprovalPolicy::Prompt)
        );
        assert_eq!(ApprovalPolicy::parse("auto"), Some(ApprovalPolicy::Auto));
        assert_eq!(ApprovalPolicy::parse("unknown"), None);
    }

    #[test]
    fn parses_known_verification_policies() {
        assert_eq!(
            VerificationPolicy::parse("off"),
            Some(VerificationPolicy::Off)
        );
        assert_eq!(
            VerificationPolicy::parse("annotate"),
            Some(VerificationPolicy::Annotate)
        );
        assert_eq!(
            VerificationPolicy::parse("require"),
            Some(VerificationPolicy::Require)
        );
        assert_eq!(VerificationPolicy::parse("unknown"), None);
    }
}
