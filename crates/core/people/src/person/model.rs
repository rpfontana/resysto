use std::collections::BTreeSet;

use resysto_model::input::{nullable, present, trimmed};
use resysto_model::{Email, OrganizationId, Record};
use serde::{Deserialize, Serialize};
use surrealdb::types::SurrealValue;
use validator::{Validate, ValidationError};

#[derive(Debug, Clone, PartialEq, Serialize, SurrealValue)]
pub struct Person {
    pub first_name: String,
    pub last_name: String,
    pub email: Option<Email>,
    pub phone: Option<String>,
    pub job_title: Option<String>,
    pub department: Option<String>,
    pub organization_ids: Vec<OrganizationId>,
}

pub type PersonRecord = Record<resysto_model::Person, Person>;

#[derive(Debug, Clone, Default, Deserialize, Validate)]
#[serde(deny_unknown_fields)]
pub struct NewPerson {
    #[validate(custom(function = "person_name"))]
    pub first_name: String,
    #[validate(custom(function = "person_name"))]
    pub last_name: String,
    pub email: Option<Email>,
    #[validate(
        length(max = 50, message = "at most 50 characters"),
        custom(function = "resysto_model::input::not_blank")
    )]
    pub phone: Option<String>,
    #[validate(
        length(max = 200, message = "at most 200 characters"),
        custom(function = "resysto_model::input::not_blank")
    )]
    pub job_title: Option<String>,
    #[validate(
        length(max = 200, message = "at most 200 characters"),
        custom(function = "resysto_model::input::not_blank")
    )]
    pub department: Option<String>,
    #[validate(length(min = 1, message = "at least one organization"))]
    pub organization_ids: Vec<OrganizationId>,
}

/// The body of a JSON Merge Patch: an absent field stays, `null` clears it
#[derive(Debug, Clone, Default, PartialEq, Deserialize, Validate)]
#[serde(deny_unknown_fields)]
pub struct PersonPatch {
    #[serde(default, deserialize_with = "present")]
    #[validate(custom(function = "person_name"))]
    pub first_name: Option<String>,
    #[serde(default, deserialize_with = "present")]
    #[validate(custom(function = "person_name"))]
    pub last_name: Option<String>,
    #[serde(default, deserialize_with = "nullable")]
    pub email: Option<Option<Email>>,
    #[serde(default, deserialize_with = "nullable")]
    #[validate(
        length(max = 50, message = "at most 50 characters"),
        custom(function = "resysto_model::input::not_blank")
    )]
    pub phone: Option<Option<String>>,
    #[serde(default, deserialize_with = "nullable")]
    #[validate(
        length(max = 200, message = "at most 200 characters"),
        custom(function = "resysto_model::input::not_blank")
    )]
    pub job_title: Option<Option<String>>,
    #[serde(default, deserialize_with = "nullable")]
    #[validate(
        length(max = 200, message = "at most 200 characters"),
        custom(function = "resysto_model::input::not_blank")
    )]
    pub department: Option<Option<String>>,
    #[serde(default, deserialize_with = "present")]
    #[validate(length(min = 1, message = "at least one organization"))]
    pub organization_ids: Option<Vec<OrganizationId>>,
}

impl From<NewPerson> for Person {
    fn from(input: NewPerson) -> Self {
        Self {
            first_name: input.first_name.trim().to_owned(),
            last_name: input.last_name.trim().to_owned(),
            email: input.email,
            phone: trimmed(input.phone),
            job_title: trimmed(input.job_title),
            department: trimmed(input.department),
            organization_ids: distinct(input.organization_ids),
        }
    }
}

impl PersonPatch {
    pub(super) fn apply(self, person: &mut Person) {
        if let Some(first_name) = self.first_name {
            person.first_name = first_name.trim().to_owned();
        }
        if let Some(last_name) = self.last_name {
            person.last_name = last_name.trim().to_owned();
        }
        if let Some(email) = self.email {
            person.email = email;
        }
        if let Some(phone) = self.phone {
            person.phone = trimmed(phone);
        }
        if let Some(job_title) = self.job_title {
            person.job_title = trimmed(job_title);
        }
        if let Some(department) = self.department {
            person.department = trimmed(department);
        }
        if let Some(organization_ids) = self.organization_ids {
            person.organization_ids = distinct(organization_ids);
        }
    }
}

fn distinct(ids: Vec<OrganizationId>) -> Vec<OrganizationId> {
    let mut seen = BTreeSet::new();
    ids.into_iter().filter(|id| seen.insert(*id)).collect()
}

fn person_name(value: &str) -> Result<(), ValidationError> {
    let length = value.trim().chars().count();
    if (1..=100).contains(&length) {
        Ok(())
    } else {
        Err(ValidationError::new("name").with_message("from 1 to 100 characters".into()))
    }
}

#[cfg(test)]
mod tests {
    use resysto_model::PersonId;
    use serde_json::json;

    use super::*;

    #[test]
    fn a_name_has_from_1_to_100_characters_once_trimmed() {
        let longest = format!("  {}  ", "à".repeat(100));
        let too_long = "à".repeat(101);
        for name in ["Ada", longest.as_str()] {
            assert!(person_name(name).is_ok(), "refused {name:?}");
        }
        for name in ["", "   ", too_long.as_str()] {
            assert!(person_name(name).is_err(), "accepted {name:?}");
        }
    }

    #[test]
    fn a_patch_keeps_absent_fields_clears_nulls_and_sets_values() {
        let acme = OrganizationId::new();
        let roma = OrganizationId::new();
        let mut person = Person {
            first_name: "Ada".into(),
            last_name: "Lovelace".into(),
            email: Some("ada@acme.it".parse().unwrap()),
            phone: Some("06 123".into()),
            job_title: None,
            department: Some("IT".into()),
            organization_ids: vec![acme],
        };
        let patch: PersonPatch = serde_json::from_value(json!({
            "last_name": " Byron ",
            "phone": null,
            "job_title": " CISO ",
            "organization_ids": [roma, acme, roma],
        }))
        .unwrap();

        patch.apply(&mut person);

        assert_eq!(person.first_name, "Ada");
        assert_eq!(person.last_name, "Byron");
        assert_eq!(
            person.email.as_ref().map(Email::as_str),
            Some("ada@acme.it")
        );
        assert_eq!(person.phone, None);
        assert_eq!(person.job_title.as_deref(), Some("CISO"));
        assert_eq!(person.department.as_deref(), Some("IT"));
        assert_eq!(person.organization_ids, vec![roma, acme]);
    }

    #[test]
    fn a_new_person_is_trimmed_and_names_each_organization_once() {
        let acme = OrganizationId::new();
        let input: NewPerson = serde_json::from_value(json!({
            "first_name": " Ada ",
            "last_name": "Lovelace ",
            "job_title": " CISO ",
            "organization_ids": [acme, acme],
        }))
        .unwrap();

        let person = Person::from(input);

        assert_eq!(person.first_name, "Ada");
        assert_eq!(person.last_name, "Lovelace");
        assert_eq!(person.job_title.as_deref(), Some("CISO"));
        assert_eq!(person.organization_ids, vec![acme]);
    }

    #[test]
    fn an_input_refuses_what_it_cannot_apply_cleanly() {
        fn refused<T: serde::de::DeserializeOwned + Validate>(body: &serde_json::Value) -> bool {
            serde_json::from_value::<T>(body.clone())
                .map_err(|error| error.to_string())
                .and_then(|input| input.validate().map_err(|error| error.to_string()))
                .is_err()
        }
        let ada = |extra: serde_json::Value| {
            let mut body = json!({
                "first_name": "Ada",
                "last_name": "Lovelace",
                "organization_ids": [OrganizationId::new()],
            });
            if let (Some(body), Some(extra)) = (body.as_object_mut(), extra.as_object()) {
                body.extend(extra.clone());
            }
            body
        };
        assert!(
            !refused::<NewPerson>(&ada(json!({}))),
            "refused a valid person"
        );
        let new_people = [
            ada(json!({ "phnoe": "06 123" })),
            ada(json!({ "id": PersonId::new() })),
            ada(json!({ "version": 1 })),
            ada(json!({ "phone": "   " })),
            ada(json!({ "department": "" })),
        ];
        for body in new_people {
            assert!(
                refused::<NewPerson>(&body),
                "accepted the new person {body}"
            );
        }
        let patches = [
            json!({ "first_name": null }),
            json!({ "last_name": null }),
            json!({ "organization_ids": null }),
            json!({ "phnoe": "06 123" }),
            json!({ "version": 2 }),
            json!({ "phone": "   " }),
            json!({ "job_title": "" }),
        ];
        for body in patches {
            assert!(refused::<PersonPatch>(&body), "accepted the patch {body}");
        }
    }
}
