use std::io;

use snafu::Snafu;
use zed_extension_api::{Architecture, Os};

#[derive(Debug, Snafu)]
#[snafu(visibility(pub(crate)))]
pub(crate) enum Error {
    #[snafu(display("Cannot read Zed VsRocq settings"))]
    ReadSettings { source: HostError },

    #[snafu(display("Cannot decode VsRocq {section}"))]
    DecodeConfiguration {
        section: &'static str,
        source: serde_json::Error,
    },

    #[snafu(display("Cannot encode VsRocq {section}"))]
    EncodeConfiguration {
        section: &'static str,
        source: serde_json::Error,
    },

    #[snafu(display("Invalid lsp.vsrocq configuration"))]
    Settings { source: vsrocq_settings::Error },

    #[snafu(display("vsrocq.proof must be an object"))]
    ProofSection,

    #[snafu(display("vsrocq.proof.{setting} must be true or false"))]
    ProofSetting { setting: &'static str },

    #[snafu(display("Cannot locate the VsRocq extension working directory"))]
    WorkingDirectory { source: io::Error },

    #[snafu(display("vsrocq.bridge.release_tag must not be empty"))]
    EmptyReleaseTag,

    #[snafu(display("Invalid GitHub repository {repository}; use owner/repo without a URL prefix"))]
    Repository { repository: String },

    #[snafu(display("Bridge download URLs must use HTTPS without spaces or fragments"))]
    DownloadUrl,

    #[snafu(display("Cannot get release {tag:?} from {repository}"))]
    Release {
        repository: String,
        tag: Option<String>,
        source: HostError,
    },

    #[snafu(display("Release {release} in {repository} has no {asset} asset"))]
    MissingAsset {
        repository: String,
        release: String,
        asset: String,
    },

    #[snafu(display("Cannot download the bridge to {path}"))]
    Download { path: String, source: HostError },

    #[snafu(display("Cannot make {path} executable"))]
    Executable { path: String, source: HostError },

    #[snafu(display("Cannot install {path}; stop running VsRocq sessions before replacement"))]
    Install { path: String, source: io::Error },

    #[snafu(display("Cannot record the installed bridge source"))]
    RecordSource { source: io::Error },

    #[snafu(display("Configured executable path must not be empty"))]
    EmptyExecutable,

    #[snafu(display("No bridge asset is available for {os:?}/{architecture:?}; set lsp.vsrocq.binary.path"))]
    Platform { os: Os, architecture: Architecture },
}

// Zed returns text rather than an error source. Keep that text at this SDK
// boundary.
#[derive(Debug, Snafu)]
#[snafu(display("{message}"))]
pub(crate) struct HostError {
    message: String,
}

pub(crate) fn host<T>(result: zed_extension_api::Result<T>) -> Result<T, HostError> {
    result.map_err(|message| HostError { message })
}

impl Error {
    pub(crate) fn report(&self) -> String {
        snafu::Report::from_error(self).to_string()
    }
}

impl From<Error> for String {
    fn from(error: Error) -> Self {
        error.report()
    }
}

pub(crate) type Result<T, E = Error> = std::result::Result<T, E>;
