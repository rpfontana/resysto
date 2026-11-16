use resysto_id::{Entity, Id};

/// Someone who works for or with the organizations of a tenant
#[derive(Debug)]
pub enum Person {}

impl Entity for Person {
    const TABLE: &'static str = "person";
}

pub type PersonId = Id<Person>;
