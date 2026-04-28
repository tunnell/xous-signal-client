use rkyv::{Archive, Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;

#[allow(dead_code)]
#[derive(Archive, Serialize, Deserialize, Debug)]
pub enum LinkState {
    Enabled,
    EnabledWithApproval,
    Disabled,
}

impl fmt::Display for LinkState {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{:?}", self)
    }
}

impl FromStr for LinkState {
    type Err = ();

    fn from_str(input: &str) -> Result<LinkState, Self::Err> {
        match input {
            "Enabled" => Ok(LinkState::Enabled),
            "EnabledWithApproval" => Ok(LinkState::EnabledWithApproval),
            "Disabled" => Ok(LinkState::Disabled),
            _ => Err(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_display_round_trip() {
        for s in &["Enabled", "EnabledWithApproval", "Disabled"] {
            let parsed: LinkState = s.parse().expect("known variant");
            assert_eq!(format!("{}", parsed), *s);
        }
    }

    #[test]
    fn unknown_input_errors() {
        assert!("enabled".parse::<LinkState>().is_err());
        assert!("Disable".parse::<LinkState>().is_err());
        assert!("".parse::<LinkState>().is_err());
    }
}
