//! Process settings from `RESYSTO_*` environment variables.

use std::{env, fs, io};

use config::{Config, Environment, Map};
use serde::de::DeserializeOwned;

const PREFIX: &str = "RESYSTO_";
const FILE_SUFFIX: &str = "_FILE";

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("cannot read {name}={path}")]
    File {
        name: String,
        path: String,
        source: io::Error,
    },
    #[error("{name} and {name}_FILE are both set")]
    Both { name: String },
    #[error(transparent)]
    Settings(#[from] config::ConfigError),
}

/// Deserializes `T` from the environment
pub fn load<T: DeserializeOwned>() -> Result<T, Error> {
    from_vars(env::vars())
}

fn from_vars<T: DeserializeOwned>(
    vars: impl IntoIterator<Item = (String, String)>,
) -> Result<T, Error> {
    let mut values: Map<String, String> = Map::new();
    let mut files = Vec::new();
    for (name, value) in vars {
        if !name.starts_with(PREFIX) {
            continue;
        }
        match name.strip_suffix(FILE_SUFFIX) {
            Some(base) => files.push((base.to_owned(), name, value)),
            None => {
                values.insert(name, value);
            }
        }
    }
    for (base, name, path) in files {
        if values.contains_key(&base) {
            return Err(Error::Both { name: base });
        }
        let content = fs::read_to_string(&path).map_err(|source| Error::File {
            name,
            path: path.clone(),
            source,
        })?;
        values.insert(base, content.trim_end_matches(['\n', '\r']).to_owned());
    }
    let settings = Config::builder()
        .add_source(
            Environment::with_prefix("RESYSTO")
                .prefix_separator("_")
                .separator("__")
                .source(Some(values)),
        )
        .build()?;
    Ok(settings.try_deserialize()?)
}

#[cfg(test)]
mod tests {
    use serde::Deserialize;

    use super::*;

    #[derive(Debug, PartialEq, Deserialize)]
    struct Settings {
        db: Db,
    }

    #[derive(Debug, PartialEq, Deserialize)]
    struct Db {
        migrator_user: String,
        migrator_password: String,
    }

    fn file(name: &str, content: &str) -> String {
        let path = env::temp_dir().join(format!("resysto-config-{}-{name}", std::process::id()));
        fs::write(&path, content).unwrap();
        path.to_string_lossy().into_owned()
    }

    fn vars(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs
            .iter()
            .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
            .collect()
    }

    #[test]
    fn a_file_variable_sets_the_value_without_its_trailing_newline() {
        let password = file("password", "12345\n");

        let settings: Settings = from_vars(vars(&[
            ("RESYSTO_DB__MIGRATOR_USER", "ns_migrator"),
            ("RESYSTO_DB__MIGRATOR_PASSWORD_FILE", &password),
            ("PATH", "/usr/bin"),
        ]))
        .unwrap();

        let db = Db {
            migrator_user: "ns_migrator".into(),
            migrator_password: "12345".into(),
        };
        assert_eq!(settings, Settings { db });
    }

    #[test]
    fn a_name_set_both_ways_is_rejected() {
        let password = file("both", "secret");

        let result: Result<Settings, Error> = from_vars(vars(&[
            ("RESYSTO_DB__MIGRATOR_USER", "ns_migrator"),
            ("RESYSTO_DB__MIGRATOR_PASSWORD", "inline"),
            ("RESYSTO_DB__MIGRATOR_PASSWORD_FILE", &password),
        ]));

        assert!(
            matches!(&result, Err(Error::Both { name }) if name == "RESYSTO_DB__MIGRATOR_PASSWORD"),
            "{result:?}"
        );
    }
}
