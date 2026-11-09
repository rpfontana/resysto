use crate::{Db, Error};

const TENANT: &[&str] = &[include_str!("../../../migrations/tenant/access.surql")];

impl Db {
    /// Creates `database` if missing and applies the tenant schema. Safe to repeat.
    pub async fn provision_tenant(&self, database: &str) -> Result<(), Error> {
        check_name(database)?;
        self.system
            .query(format!("DEFINE DATABASE IF NOT EXISTS `{database}`"))
            .await?
            .check()?;
        let session = self.system_session(database).await?;
        for sql in TENANT {
            session
                .query(*sql)
                .bind(("access_key", self.verifying_key.clone()))
                .await?
                .check()?;
        }
        Ok(())
    }
}

// The name is interpolated into SurrealQL as an identifier.
fn check_name(name: &str) -> Result<(), Error> {
    let valid = !name.is_empty()
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_');
    if valid {
        Ok(())
    } else {
        Err(Error::InvalidName(name.to_owned()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::{connect, info, new_tenant};

    #[tokio::test]
    async fn provisioning_again_leaves_the_schema_as_it_was() {
        let db = connect().await;
        let tenant = new_tenant(&db).await;
        let before = info(&db, &tenant).await;
        db.provision_tenant(&tenant).await.unwrap();
        assert_eq!(info(&db, &tenant).await, before);
    }

    #[test]
    fn accepts_only_plain_identifiers() {
        assert!(check_name("t_acme_0a1b2c3d").is_ok());
        for name in [
            "",
            "T_acme",
            "t-acme",
            "t acme",
            "t`; REMOVE NAMESPACE x; --",
        ] {
            assert!(check_name(name).is_err(), "accepted {name:?}");
        }
    }
}
