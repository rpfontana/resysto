use resysto_id::{Entity, Id};

/// A member of a tenant, as the tenant's database knows it; its key is its account's
#[derive(Debug)]
pub enum Member {}

impl Entity for Member {
    const TABLE: &'static str = "member";
}

pub type MemberId = Id<Member>;
