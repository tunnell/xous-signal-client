use rkyv::{Archive, Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;

// When to trust new identities:
#[allow(dead_code)]
#[derive(Archive, Serialize, Deserialize, Debug)]
pub enum TrustMode {
    ///  Trust the first seen identity key from new users, changed keys must be verified manually
    OnFirstUse,
    /// Trust any new identity key without verification
    Always,
    /// Don’t trust any unknown identity key, every key must be verified manually
    Never,
}

impl fmt::Display for TrustMode {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{:?}", self)
    }
}

impl FromStr for TrustMode {
    type Err = ();

    fn from_str(input: &str) -> Result<TrustMode, Self::Err> {
        match input {
            "OnFirstUse" => Ok(TrustMode::OnFirstUse),
            "Always" => Ok(TrustMode::Always),
            "Never" => Ok(TrustMode::Never),
            _ => Err(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_matches_variant_name() {
        assert_eq!(format!("{}", TrustMode::OnFirstUse), "OnFirstUse");
        assert_eq!(format!("{}", TrustMode::Always), "Always");
        assert_eq!(format!("{}", TrustMode::Never), "Never");
    }

    #[test]
    fn parse_then_display_round_trips_every_variant() {
        for s in &["OnFirstUse", "Always", "Never"] {
            let parsed: TrustMode = s.parse().expect("known variant");
            assert_eq!(format!("{}", parsed), *s);
        }
    }

    #[test]
    fn unknown_input_errors() {
        assert!("Sometimes".parse::<TrustMode>().is_err());
        assert!("".parse::<TrustMode>().is_err());
        assert!("onfirstuse".parse::<TrustMode>().is_err()); // case-sensitive
    }
}
