use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::{Error, Lane, ManifestFile};

const MAX_BATCH: u32 = 1000;
const STAMP_DIGITS: usize = 14;

#[derive(Debug)]
pub(crate) struct Manifest {
    pub(crate) path: &'static str,
    pub(crate) id: i64,
    pub(crate) hash: String,
    pub(crate) title: String,
    pub(crate) expand: Vec<Step>,
    pub(crate) contract: Vec<Step>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Body {
    title: String,
    #[serde(default)]
    expand: Vec<Step>,
    #[serde(default)]
    contract: Vec<Step>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Step {
    Schema(String),
    Data(Data),
    Assert(String),
    Remove(Remove),
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Data {
    pub(crate) sql: String,
    pub(crate) batch: u32,
}

/// The `REMOVE … IF EXISTS` statement built from a `remove` step
#[derive(Debug, Deserialize)]
#[serde(try_from = "RemoveSpec")]
pub(crate) struct Remove(pub(crate) String);

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RemoveSpec {
    table: String,
    field: Option<String>,
    index: Option<String>,
    event: Option<String>,
}

impl TryFrom<RemoveSpec> for Remove {
    type Error = String;

    fn try_from(spec: RemoveSpec) -> Result<Self, String> {
        let table = identifier(spec.table)?;
        let statement = match (spec.field, spec.index, spec.event) {
            (None, None, None) => format!("REMOVE TABLE IF EXISTS {table}"),
            (Some(field), None, None) => {
                format!("REMOVE FIELD IF EXISTS {} ON {table}", identifier(field)?)
            }
            (None, Some(index), None) => {
                format!("REMOVE INDEX IF EXISTS {} ON {table}", identifier(index)?)
            }
            (None, None, Some(event)) => {
                format!("REMOVE EVENT IF EXISTS {} ON {table}", identifier(event)?)
            }
            _ => return Err("remove takes at most one of field, index, event".into()),
        };
        Ok(Self(statement))
    }
}

impl Manifest {
    pub(crate) fn cites(&self, path: &str) -> bool {
        self.expand
            .iter()
            .chain(&self.contract)
            .any(|step| matches!(step, Step::Schema(cited) if cited == path))
    }
}

pub(crate) fn load(lane: &Lane) -> Result<Vec<Manifest>, Error> {
    let mut manifests: Vec<Manifest> = Vec::with_capacity(lane.manifests.len());
    for file in lane.manifests {
        let invalid = |reason| Error::InvalidManifest {
            path: file.path,
            reason,
        };
        let manifest = parse(lane, file).map_err(invalid)?;
        if let Some(previous) = manifests
            .last()
            .filter(|previous| previous.id >= manifest.id)
        {
            return Err(invalid(format!("listed after {}", previous.path)));
        }
        manifests.push(manifest);
    }
    Ok(manifests)
}

fn parse(lane: &Lane, file: &ManifestFile) -> Result<Manifest, String> {
    let id = stamp(file.path)
        .ok_or_else(|| "the name must be <UTC YYYYMMDDHHMMSS>-<title>.toml".to_owned())?;
    let body: Body = toml::from_str(file.toml).map_err(|error| error.to_string())?;
    for (section, steps) in [("expand", &body.expand), ("contract", &body.contract)] {
        for (step, n) in steps.iter().zip(1..) {
            check(lane, section, step).map_err(|reason| format!("{section} step {n}: {reason}"))?;
        }
    }
    Ok(Manifest {
        path: file.path,
        id,
        hash: format!("{:x}", Sha256::digest(file.toml)),
        title: body.title,
        expand: body.expand,
        contract: body.contract,
    })
}

fn check(lane: &Lane, section: &str, step: &Step) -> Result<(), String> {
    match step {
        Step::Schema(path) if !lane.schema.iter().any(|file| file.path == path) => {
            Err(format!("{path} is not a file of the lane's schema/"))
        }
        Step::Data(data) if !(1..=MAX_BATCH).contains(&data.batch) => {
            Err(format!("batch must be between 1 and {MAX_BATCH}"))
        }
        Step::Data(data) if !(data.sql.contains("$range") && data.sql.contains("$limit")) => {
            Err("data must read $range and $limit".into())
        }
        // without a final RETURN of the ids, write_in_batches stops after one batch
        Step::Data(data) if !last_statement_is_return(&data.sql) => {
            Err("data must end with a RETURN of the batch's ids".into())
        }
        Step::Remove(_) if section == "expand" => Err("remove is only allowed in contract".into()),
        _ => Ok(()),
    }
}

fn stamp(path: &str) -> Option<i64> {
    let (stamp, title) = path.strip_suffix(".toml")?.split_once('-')?;
    let valid = stamp.len() == STAMP_DIGITS
        && stamp.bytes().all(|b| b.is_ascii_digit())
        && !title.is_empty();
    valid.then(|| stamp.parse().ok()).flatten()
}

fn last_statement_is_return(sql: &str) -> bool {
    let last = sql
        .trim_end()
        .trim_end_matches(';')
        .rsplit(';')
        .next()
        .unwrap_or_default();
    last.trim_start()
        .get(..6)
        .is_some_and(|word| word.eq_ignore_ascii_case("return"))
}

fn identifier(name: String) -> Result<String, String> {
    let plain = name.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_')
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
    if plain {
        Ok(name)
    } else {
        Err(format!("{name:?} is not a plain identifier"))
    }
}

#[cfg(test)]
mod tests {
    use resysto_surrealdb::SchemaFile;

    use super::*;

    const VALID: &str = r#"
        title = "person.n from n_old"

        [[expand]]
        schema = "person.surql"

        [[expand]]
        data = { sql = """
            LET $batch = (SELECT VALUE id FROM type::record('person', $range) LIMIT $limit);
            UPDATE $batch SET n = n_old RETURN NONE;
            RETURN $batch;
        """, batch = 500 }

        [[contract]]
        remove = { table = "person", field = "n_old" }
    "#;

    const PATH: &str = "20261112143000-n.toml";

    fn load_lane(files: &[(&'static str, String)]) -> Result<Vec<Manifest>, Error> {
        let manifests = files
            .iter()
            .map(|(path, toml)| ManifestFile {
                path,
                toml: toml.clone().leak(),
            })
            .collect();
        load(&Lane {
            schema: &[SchemaFile {
                path: "person.surql",
                sql: "",
            }],
            manifests: Vec::leak(manifests),
        })
    }

    #[test]
    fn a_malformed_manifest_is_rejected() {
        load_lane(&[(PATH, VALID.into())]).unwrap();

        let one = |toml: String| vec![(PATH, toml)];
        let cases = [
            vec![("0001-n.toml", VALID.into())],
            vec![("20261112143000.toml", VALID.into())],
            vec![("2026111214300a-n.toml", VALID.into())],
            vec![(PATH, VALID.into()), (PATH, VALID.into())],
            vec![
                (PATH, VALID.into()),
                ("20261112142959-n.toml", VALID.into()),
            ],
            one(VALID.replace("title", "id = 1\ntitle")),
            one(VALID.replace("\"person.surql\"", "\"missing.surql\"")),
            one(VALID.replace("type::record('person', $range)", "person")),
            one(VALID.replace("RETURN $batch;", "")),
            one(VALID.replace("batch = 500", "batch = 1001")),
            one(VALID.replace("[[contract]]", "[[expand]]")),
            one(VALID.replace("field = \"n_old\"", "field = \"n_old\", index = \"n\"")),
            one(VALID.replace("\"n_old\"", "\"n_old; REMOVE TABLE x\"")),
        ];
        for files in cases {
            let result = load_lane(&files);
            assert!(
                matches!(result, Err(Error::InvalidManifest { .. })),
                "accepted {files:?}"
            );
        }
    }
}
