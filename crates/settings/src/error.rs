use std::sync::Arc;

use snafu::Snafu;

/// A failure to parse, merge, validate, or serialize settings.
#[derive(Debug, Snafu)]
#[snafu(visibility(pub(crate)))]
pub enum Error {
    /// The VsRocq settings are not an object.
    #[snafu(display("could not parse VsRocq settings: {source}"))]
    ParseSettings { source: serde_json::Error },

    /// A known field has the wrong type or value after all layers are merged.
    /// The shared source also survives copies made before validation.
    #[snafu(display("{field}: {source}"))]
    InvalidSetting {
        field: &'static str,
        source: Arc<serde_json::Error>,
    },

    /// A missing field was serialized without an omission rule.
    #[snafu(display("missing configuration field was not omitted"))]
    MissingSetting,

    /// An unknown field could not be read as an object during a merge.
    #[snafu(display("could not parse unknown setting object {field:?}: {source}"))]
    ParseUnknownObject { field: String, source: serde_json::Error },

    /// A merged unknown object could not be stored as raw JSON.
    #[snafu(display("could not serialize unknown setting object {field:?}: {source}"))]
    SerializeUnknownObject { field: String, source: serde_json::Error },
}

/// A settings operation with a typed failure.
pub type Result<T, E = Error> = std::result::Result<T, E>;
