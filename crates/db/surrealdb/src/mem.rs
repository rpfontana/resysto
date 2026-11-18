use surrealdb::engine::any;

use crate::{Config, Db, Error};

const ENDPOINT: &str = "mem://";

const SETUP: &str = "DEFINE NAMESPACE test; USE NS test;
    DEFINE USER owner ON NAMESPACE PASSWORD 'owner' ROLES OWNER;";

const SIGNING_KEY: &str = "-----BEGIN PRIVATE KEY-----
MIGHAgEAMBMGByqGSM49AgEGCCqGSM49AwEHBG0wawIBAQQgCcu34KTFnm0J5qWJ
AxhAcHSbqFj+B/am0htHlDufqWihRANCAASN/BWdDI7GaVVD6HAh20C4ZzJSHSsz
9htiM2MVKHWJFnxAy4GKb6YFXGV6KEDsrn5MQIa7TTsyRDnIcCjQztXz
-----END PRIVATE KEY-----
";
const VERIFYING_KEY: &str = "-----BEGIN PUBLIC KEY-----
MFkwEwYHKoZIzj0CAQYIKoZIzj0DAQcDQgAEjfwVnQyOxmlVQ+hwIdtAuGcyUh0r
M/YbYjNjFSh1iRZ8QMuBim+mBVxleihA7K5+TECGu007MkQ5yHAo0M7V8w==
-----END PUBLIC KEY-----
";

impl Db {
    /// in memory db for test purposes
    pub async fn memory() -> Result<Self, Error> {
        let bare = any::connect(ENDPOINT).await?;
        bare.clone().query(SETUP).await?.check()?;
        Self::open(
            bare,
            Config {
                endpoint: ENDPOINT.into(),
                namespace: "test".into(),
                username: "owner".into(),
                password: "owner".into(),
                signing_key: SIGNING_KEY.into(),
                verifying_key: VERIFYING_KEY.into(),
            },
        )
        .await
    }
}
