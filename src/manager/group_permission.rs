use rkyv::{Archive, Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;

#[allow(dead_code)]
#[derive(Archive, Serialize, Deserialize, Debug)]
pub enum GroupPermission {
    EveryMember,
    OnlyAdmins,
}

impl fmt::Display for GroupPermission {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{:?}", self)
    }
}

impl FromStr for GroupPermission {
    type Err = ();

    fn from_str(input: &str) -> Result<GroupPermission, Self::Err> {
        match input {
            "EveryMember" => Ok(GroupPermission::EveryMember),
            "OnlyAdmins" => Ok(GroupPermission::OnlyAdmins),
            _ => Err(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_display_round_trip() {
        for s in &["EveryMember", "OnlyAdmins"] {
            let parsed: GroupPermission = s.parse().expect("known variant");
            assert_eq!(format!("{}", parsed), *s);
        }
    }

    #[test]
    fn unknown_input_errors() {
        assert!("everymember".parse::<GroupPermission>().is_err());
        assert!("Admins".parse::<GroupPermission>().is_err());
    }
}
