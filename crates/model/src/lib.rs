//! The vocabulary the bounded contexts share: entity markers, their ids and value objects

mod country;
mod email;
mod organization;
mod person;

pub use country::{Country, ParseCountryError};
pub use email::{Email, ParseEmailError};
pub use organization::{Organization, OrganizationId};
pub use person::{Person, PersonId};
