//! Shared response/request DTOs that are part of the public contract before
//! the endpoints using them land.

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// Interface/notification language. English is the fallback. A language
/// choice never influences authorization or business rules.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum Language {
    #[default]
    En,
    De,
}

impl Language {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::En => "en",
            Self::De => "de",
        }
    }

    /// Parses a stored/declared language code; unknown values → `None`.
    pub fn parse(code: &str) -> Option<Self> {
        match code {
            "en" => Some(Self::En),
            "de" => Some(Self::De),
            _ => None,
        }
    }
}

/// Where a ticket currently is, as shown to its requester.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum AssignmentQueue {
    /// In the shared general intake queue: no team, no assignee.
    GeneralQueue,
    /// Dispatched to a team (`assignee` may still be `null` = unassigned).
    Team,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, ToSchema)]
pub struct PublicTeam {
    pub name: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct PublicAssignee {
    pub display_name: String,
}

/// Public assignment details for requester-facing ticket responses.
///
/// Deliberately contains only display values: no team/account ids,
/// usernames, emails, or roles. `team` and `assignee` are always present and
/// `null` when not set, so "general queue" and "unassigned" are explicit.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, ToSchema)]
pub struct PublicTicketAssignment {
    pub queue: AssignmentQueue,
    #[schema(required = true)]
    pub team: Option<PublicTeam>,
    #[schema(required = true)]
    pub assignee: Option<PublicAssignee>,
}

impl PublicTicketAssignment {
    pub fn general_queue() -> Self {
        Self {
            queue: AssignmentQueue::GeneralQueue,
            team: None,
            assignee: None,
        }
    }

    /// Builds the public view from the current team name and assignee display
    /// name. Without a team the ticket is in the general queue, and any
    /// assignee is ignored (general-queue tickets never have one).
    pub fn from_parts(team_name: Option<String>, assignee_display_name: Option<String>) -> Self {
        match team_name {
            None => Self::general_queue(),
            Some(name) => Self {
                queue: AssignmentQueue::Team,
                team: Some(PublicTeam { name }),
                assignee: assignee_display_name.map(|display_name| PublicAssignee { display_name }),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn general_queue_is_explicit() {
        let value =
            serde_json::to_value(PublicTicketAssignment::from_parts(None, Some("x".into())))
                .unwrap();
        assert_eq!(
            value,
            json!({"queue": "general_queue", "team": null, "assignee": null})
        );
    }

    #[test]
    fn team_without_assignee_is_explicitly_unassigned() {
        let value = serde_json::to_value(PublicTicketAssignment::from_parts(
            Some("Network".into()),
            None,
        ))
        .unwrap();
        assert_eq!(
            value,
            json!({"queue": "team", "team": {"name": "Network"}, "assignee": null})
        );
    }

    #[test]
    fn assigned_exposes_only_display_values() {
        let value = serde_json::to_value(PublicTicketAssignment::from_parts(
            Some("Network".into()),
            Some("Ada L.".into()),
        ))
        .unwrap();
        assert_eq!(
            value,
            json!({"queue": "team", "team": {"name": "Network"}, "assignee": {"displayName": "Ada L."}})
        );
        // Guard against fields being added later: only these keys may exist.
        let keys = |v: &serde_json::Value| -> Vec<String> {
            let mut keys: Vec<String> = v.as_object().unwrap().keys().cloned().collect();
            keys.sort();
            keys
        };
        assert_eq!(keys(&value), ["assignee", "queue", "team"]);
        assert_eq!(keys(&value["team"]), ["name"]);
        assert_eq!(keys(&value["assignee"]), ["displayName"]);
    }

    #[test]
    fn language_codes() {
        assert_eq!(serde_json::to_value(Language::De).unwrap(), json!("de"));
        assert_eq!(Language::parse("en"), Some(Language::En));
        assert_eq!(Language::parse("fr"), None);
        assert_eq!(Language::default(), Language::En);
        assert!(serde_json::from_value::<Language>(json!("EN")).is_err());
    }
}
