use resysto_id::{Entity, Id};

/// The unit every record of a tenant belongs to
#[derive(Debug)]
pub enum Organization {}

impl Entity for Organization {
    const TABLE: &'static str = "organization";
}

pub type OrganizationId = Id<Organization>;
