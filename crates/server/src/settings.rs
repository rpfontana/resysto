use serde::Deserialize;

#[derive(Deserialize)]
pub(crate) struct Settings {
    pub(crate) addr: String,
    pub(crate) site: String,
}
