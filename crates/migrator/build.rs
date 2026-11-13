use std::path::{Path, PathBuf};
use std::{env, fs, io};

const LANES: [&str; 2] = ["master", "tenant"];

fn main() -> io::Result<()> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../migrations");
    rerun_if_changed(&root);
    let mut lanes = String::new();
    for name in LANES {
        lanes.push_str(&lane(name, &root.join(name))?);
    }
    let out = env::var_os("OUT_DIR").ok_or_else(|| io::Error::other("OUT_DIR is not set"))?;
    fs::write(PathBuf::from(out).join("lanes.rs"), lanes)
}

fn lane(name: &str, dir: &Path) -> io::Result<String> {
    let schema_dir = dir.join("schema");
    let schema: String = files(&schema_dir, "surql")?
        .iter()
        .map(|path| {
            let source = schema_dir.join(path).display().to_string();
            format!("SchemaFile {{ path: {path:?}, sql: include_str!({source:?}) }},\n")
        })
        .collect();
    let manifests: String = files(dir, "toml")?
        .iter()
        .map(|path| {
            let source = dir.join(path).display().to_string();
            format!("ManifestFile {{ path: {path:?}, toml: include_str!({source:?}) }},\n")
        })
        .collect();
    Ok(format!(
        "const {}: Lane = Lane {{\nschema: &[\n{schema}],\nmanifests: &[\n{manifests}],\n}};\n",
        name.to_uppercase()
    ))
}

// adding or removing a file changes its directory's mtime
fn rerun_if_changed(dir: &Path) {
    println!("cargo:rerun-if-changed={}", dir.display());
}

fn files(dir: &Path, extension: &str) -> io::Result<Vec<String>> {
    let mut found = Vec::new();
    walk(dir, dir, extension, &mut found)?;
    found.sort();
    Ok(found)
}

fn walk(root: &Path, dir: &Path, extension: &str, found: &mut Vec<String>) -> io::Result<()> {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    rerun_if_changed(dir);
    for entry in entries {
        let path = entry?.path();
        if path.is_dir() {
            walk(root, &path, extension, found)?;
        } else if path.extension().is_some_and(|ext| ext == extension) {
            let relative = path.strip_prefix(root).map_err(io::Error::other)?;
            found.push(relative.to_string_lossy().replace('\\', "/"));
        }
    }
    Ok(())
}
