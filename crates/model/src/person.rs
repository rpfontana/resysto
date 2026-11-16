use resysto_id::{Entity, Id};

#[derive(Debug)]
pub enum Person {}

impl Entity for Person {
    const TABLE: &'static str = "person";
}

pub type PersonId = Id<Person>;
