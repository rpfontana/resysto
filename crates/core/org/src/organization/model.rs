//! Organizations: the types and the rules of the tree

use resysto_model::input::{nullable, present, trimmed};
use resysto_model::{Country, Email, OrganizationId, Record};
use serde::{Deserialize, Serialize};
use surrealdb::types::SurrealValue;
use validator::{Validate, ValidationError};

/// How many levels an organization can sit below its root
pub const MAX_DEPTH: usize = 5;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, SurrealValue)]
#[serde(rename_all = "snake_case")]
// untagged writes each variant as its name, the form the schema's literal union expects
#[surreal(untagged, rename_all = "snake_case")]
pub enum Industry {
    Technology,
    Finance,
    Healthcare,
    Manufacturing,
    Retail,
    Education,
    Government,
    Energy,
    Transportation,
    Telecommunications,
    RealEstate,
    Agriculture,
    Entertainment,
    Hospitality,
    Other,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, SurrealValue)]
pub struct Organization {
    pub name: String,
    pub parent: Option<OrganizationId>,
    /// The ancestors from the root down to the parent
    pub path: Vec<OrganizationId>,
    pub active: bool,
    pub org_type: Option<String>,
    pub description: Option<String>,
    pub notes: Option<String>,
    pub email: Option<Email>,
    pub phone: Option<String>,
    pub website: Option<String>,
    pub address: Option<String>,
    pub city: Option<String>,
    pub postal_code: Option<String>,
    pub country: Option<Country>,
    pub vat_number: Option<String>,
    pub industry: Option<Industry>,
    pub annual_income: Option<i64>,
    pub employee_count: Option<i64>,
}

pub type OrganizationRecord = Record<resysto_model::Organization, Organization>;

/// The body of a create
#[derive(Debug, Clone, Default, Deserialize, Validate)]
#[serde(deny_unknown_fields)]
pub struct NewOrganization {
    #[validate(custom(function = "organization_name"))]
    pub name: String,
    pub parent: Option<OrganizationId>,
    /// `true` when absent
    pub active: Option<bool>,
    #[validate(
        length(max = 100, message = "at most 100 characters"),
        custom(function = "resysto_model::input::not_blank")
    )]
    pub org_type: Option<String>,
    #[validate(
        length(max = 2000, message = "at most 2000 characters"),
        custom(function = "resysto_model::input::not_blank")
    )]
    pub description: Option<String>,
    #[validate(
        length(max = 10000, message = "at most 10000 characters"),
        custom(function = "resysto_model::input::not_blank")
    )]
    pub notes: Option<String>,
    pub email: Option<Email>,
    #[validate(
        length(max = 50, message = "at most 50 characters"),
        custom(function = "resysto_model::input::not_blank")
    )]
    pub phone: Option<String>,
    #[validate(
        length(max = 500, message = "at most 500 characters"),
        custom(function = "resysto_model::input::not_blank")
    )]
    pub website: Option<String>,
    #[validate(
        length(max = 500, message = "at most 500 characters"),
        custom(function = "resysto_model::input::not_blank")
    )]
    pub address: Option<String>,
    #[validate(
        length(max = 200, message = "at most 200 characters"),
        custom(function = "resysto_model::input::not_blank")
    )]
    pub city: Option<String>,
    #[validate(
        length(max = 20, message = "at most 20 characters"),
        custom(function = "resysto_model::input::not_blank")
    )]
    pub postal_code: Option<String>,
    pub country: Option<Country>,
    #[validate(
        length(max = 50, message = "at most 50 characters"),
        custom(function = "resysto_model::input::not_blank")
    )]
    pub vat_number: Option<String>,
    pub industry: Option<Industry>,
    #[validate(range(min = 1, message = "must be at least 1"))]
    pub annual_income: Option<i64>,
    #[validate(range(min = 1, message = "must be at least 1"))]
    pub employee_count: Option<i64>,
}

/// The body of a JSON Merge Patch: an absent field stays, `null` clears it
#[derive(Debug, Clone, Default, PartialEq, Deserialize, Validate)]
#[serde(deny_unknown_fields)]
pub struct OrganizationPatch {
    #[serde(default, deserialize_with = "present")]
    #[validate(custom(function = "organization_name"))]
    pub name: Option<String>,
    /// `null` moves the organization and its branch to the root
    #[serde(default, deserialize_with = "nullable")]
    pub parent: Option<Option<OrganizationId>>,
    #[serde(default, deserialize_with = "present")]
    pub active: Option<bool>,
    #[serde(default, deserialize_with = "nullable")]
    #[validate(
        length(max = 100, message = "at most 100 characters"),
        custom(function = "resysto_model::input::not_blank")
    )]
    pub org_type: Option<Option<String>>,
    #[serde(default, deserialize_with = "nullable")]
    #[validate(
        length(max = 2000, message = "at most 2000 characters"),
        custom(function = "resysto_model::input::not_blank")
    )]
    pub description: Option<Option<String>>,
    #[serde(default, deserialize_with = "nullable")]
    #[validate(
        length(max = 10000, message = "at most 10000 characters"),
        custom(function = "resysto_model::input::not_blank")
    )]
    pub notes: Option<Option<String>>,
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
        length(max = 500, message = "at most 500 characters"),
        custom(function = "resysto_model::input::not_blank")
    )]
    pub website: Option<Option<String>>,
    #[serde(default, deserialize_with = "nullable")]
    #[validate(
        length(max = 500, message = "at most 500 characters"),
        custom(function = "resysto_model::input::not_blank")
    )]
    pub address: Option<Option<String>>,
    #[serde(default, deserialize_with = "nullable")]
    #[validate(
        length(max = 200, message = "at most 200 characters"),
        custom(function = "resysto_model::input::not_blank")
    )]
    pub city: Option<Option<String>>,
    #[serde(default, deserialize_with = "nullable")]
    #[validate(
        length(max = 20, message = "at most 20 characters"),
        custom(function = "resysto_model::input::not_blank")
    )]
    pub postal_code: Option<Option<String>>,
    #[serde(default, deserialize_with = "nullable")]
    pub country: Option<Option<Country>>,
    #[serde(default, deserialize_with = "nullable")]
    #[validate(
        length(max = 50, message = "at most 50 characters"),
        custom(function = "resysto_model::input::not_blank")
    )]
    pub vat_number: Option<Option<String>>,
    #[serde(default, deserialize_with = "nullable")]
    pub industry: Option<Option<Industry>>,
    #[serde(default, deserialize_with = "nullable")]
    #[validate(range(min = 1, message = "must be at least 1"))]
    pub annual_income: Option<Option<i64>>,
    #[serde(default, deserialize_with = "nullable")]
    #[validate(range(min = 1, message = "must be at least 1"))]
    pub employee_count: Option<Option<i64>>,
}

impl NewOrganization {
    /// The organization this input describes, at `path` in the tree
    pub(super) fn into_organization(self, path: Vec<OrganizationId>) -> Organization {
        Organization {
            name: self.name.trim().to_owned(),
            parent: self.parent,
            path,
            active: self.active.unwrap_or(true),
            org_type: trimmed(self.org_type),
            description: trimmed(self.description),
            notes: trimmed(self.notes),
            email: self.email,
            phone: trimmed(self.phone),
            website: trimmed(self.website),
            address: trimmed(self.address),
            city: trimmed(self.city),
            postal_code: trimmed(self.postal_code),
            country: self.country,
            vat_number: trimmed(self.vat_number),
            industry: self.industry,
            annual_income: self.annual_income,
            employee_count: self.employee_count,
        }
    }
}

impl OrganizationPatch {
    // every field but the parent, which moves the branch and is handled before
    pub(super) fn apply(self, organization: &mut Organization) {
        if let Some(name) = self.name {
            organization.name = name.trim().to_owned();
        }
        if let Some(active) = self.active {
            organization.active = active;
        }
        if let Some(org_type) = self.org_type {
            organization.org_type = trimmed(org_type);
        }
        if let Some(description) = self.description {
            organization.description = trimmed(description);
        }
        if let Some(notes) = self.notes {
            organization.notes = trimmed(notes);
        }
        if let Some(email) = self.email {
            organization.email = email;
        }
        if let Some(phone) = self.phone {
            organization.phone = trimmed(phone);
        }
        if let Some(website) = self.website {
            organization.website = trimmed(website);
        }
        if let Some(address) = self.address {
            organization.address = trimmed(address);
        }
        if let Some(city) = self.city {
            organization.city = trimmed(city);
        }
        if let Some(postal_code) = self.postal_code {
            organization.postal_code = trimmed(postal_code);
        }
        if let Some(country) = self.country {
            organization.country = country;
        }
        if let Some(vat_number) = self.vat_number {
            organization.vat_number = trimmed(vat_number);
        }
        if let Some(industry) = self.industry {
            organization.industry = industry;
        }
        if let Some(annual_income) = self.annual_income {
            organization.annual_income = annual_income;
        }
        if let Some(employee_count) = self.employee_count {
            organization.employee_count = employee_count;
        }
    }
}

fn organization_name(value: &str) -> Result<(), ValidationError> {
    let length = value.trim().chars().count();
    if (2..=200).contains(&length) {
        Ok(())
    } else {
        Err(ValidationError::new("name").with_message("from 2 to 200 characters".into()))
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn a_name_has_from_2_to_200_characters_once_trimmed() {
        let longest = format!("  {}  ", "à".repeat(200));
        let too_long = "à".repeat(201);
        for name in ["Ab", longest.as_str()] {
            assert!(organization_name(name).is_ok(), "refused {name:?}");
        }
        for name in ["", " A ", too_long.as_str()] {
            assert!(organization_name(name).is_err(), "accepted {name:?}");
        }
    }

    #[test]
    fn a_patch_keeps_absent_fields_clears_nulls_and_sets_values() {
        let mut organization = Organization {
            name: "acme".into(),
            email: Some("info@acme.it".parse().unwrap()),
            phone: Some("06 123".into()),
            ..Default::default()
        };
        let patch: OrganizationPatch = serde_json::from_value(json!({
            "name": " Acme Spa ",
            "phone": null,
            "city": " Roma ",
            "industry": "real_estate",
        }))
        .unwrap();

        patch.apply(&mut organization);

        assert_eq!(organization.name, "Acme Spa");
        assert_eq!(
            organization.email.as_ref().map(Email::as_str),
            Some("info@acme.it")
        );
        assert_eq!(organization.phone, None);
        assert_eq!(organization.city.as_deref(), Some("Roma"));
        assert_eq!(organization.industry, Some(Industry::RealEstate));
    }

    #[test]
    fn an_input_refuses_what_it_cannot_apply_cleanly() {
        fn refused<T: serde::de::DeserializeOwned + Validate>(body: &serde_json::Value) -> bool {
            serde_json::from_value::<T>(body.clone())
                .map_err(|error| error.to_string())
                .and_then(|input| input.validate().map_err(|error| error.to_string()))
                .is_err()
        }
        assert!(
            !refused::<NewOrganization>(&json!({ "name": "acme" })),
            "refused a valid organization"
        );
        let new_organizations = [
            json!({ "name": "acme", "path": [] }),
            json!({ "name": "acme", "version": 1 }),
            json!({ "name": "acme", "city": "  " }),
            json!({ "name": "acme", "notes": "" }),
        ];
        for body in new_organizations {
            assert!(
                refused::<NewOrganization>(&body),
                "accepted the new organization {body}"
            );
        }
        let patches = [
            json!({ "name": null }),
            json!({ "active": null }),
            json!({ "path": [] }),
            json!({ "version": 2 }),
            json!({ "city": "  " }),
            json!({ "notes": "" }),
        ];
        for body in patches {
            assert!(
                refused::<OrganizationPatch>(&body),
                "accepted the patch {body}"
            );
        }
    }
}
