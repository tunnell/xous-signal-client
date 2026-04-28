use std::fmt;
use std::str::FromStr;

// The server environment to use:
#[derive(Clone, Debug)]
pub enum ServiceEnvironment {
    Live,
    Staging,
}

impl fmt::Display for ServiceEnvironment {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{:?}", self)
    }
}

impl FromStr for ServiceEnvironment {
    type Err = ();

    fn from_str(input: &str) -> Result<ServiceEnvironment, Self::Err> {
        match input {
            "Live" => Ok(ServiceEnvironment::Live),
            "Staging" => Ok(ServiceEnvironment::Staging),
            _ => Err(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_display_round_trip() {
        for s in &["Live", "Staging"] {
            let parsed: ServiceEnvironment = s.parse().expect("known variant");
            assert_eq!(format!("{}", parsed), *s);
        }
    }

    #[test]
    fn unknown_input_errors() {
        assert!("Production".parse::<ServiceEnvironment>().is_err());
        assert!("live".parse::<ServiceEnvironment>().is_err());
    }
}
