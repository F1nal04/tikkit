//! Small shared domain enums used across modules (auth, authorization,
//! administration). Their serialized forms are the stable snake_case machine
//! identifiers stored in the database and used by the API.

use std::fmt;

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// Account role. Exactly one per account; see spec.md "Roles".
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    User,
    Operator,
    Specialist,
    TeamLead,
    Admin,
}

impl Role {
    pub const ALL: [Role; 5] = [
        Role::User,
        Role::Operator,
        Role::Specialist,
        Role::TeamLead,
        Role::Admin,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Operator => "operator",
            Self::Specialist => "specialist",
            Self::TeamLead => "team_lead",
            Self::Admin => "admin",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|role| role.as_str() == value)
    }

    /// Roles that do help-desk work (general queue access): everything but
    /// User.
    pub const fn is_staff(self) -> bool {
        !matches!(self, Self::User)
    }

    /// Roles that may belong to a team and claim or be assigned team
    /// tickets. Admins are excluded: they act through the Admin role, never
    /// through team membership (spec "Roles and team structure").
    pub const fn is_team_eligible(self) -> bool {
        matches!(self, Self::Operator | Self::Specialist | Self::TeamLead)
    }
}

impl fmt::Display for Role {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Error for unknown stored enum values (a schema/code mismatch).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnknownValue(pub String);

impl fmt::Display for UnknownValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "unknown stored value {:?}", self.0)
    }
}

impl std::error::Error for UnknownValue {}

/// `#[sqlx(try_from = "String")]` support for `FromRow` structs.
impl TryFrom<String> for Role {
    type Error = UnknownValue;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::parse(&value).ok_or(UnknownValue(value))
    }
}

/// Account lifecycle status.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum AccountStatus {
    Active,
    Deactivated,
}

impl AccountStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Deactivated => "deactivated",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "active" => Some(Self::Active),
            "deactivated" => Some(Self::Deactivated),
            _ => None,
        }
    }
}

impl TryFrom<String> for AccountStatus {
    type Error = UnknownValue;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::parse(&value).ok_or(UnknownValue(value))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn role_identifiers_are_stable() {
        for role in Role::ALL {
            assert_eq!(Role::parse(role.as_str()), Some(role));
            assert_eq!(
                serde_json::to_value(role).unwrap(),
                serde_json::Value::String(role.as_str().into())
            );
        }
        assert_eq!(Role::parse("Admin"), None);
        assert_eq!(
            AccountStatus::parse("deactivated"),
            Some(AccountStatus::Deactivated)
        );
    }
}
