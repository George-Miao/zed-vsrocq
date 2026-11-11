use std::{
    env, fs,
    time::{SystemTime, UNIX_EPOCH},
};

use snafu::{OptionExt, ResultExt, ensure};
use zed_extension_api::{
    self as zed, DownloadedFileType, GithubReleaseOptions, LanguageServerId,
    LanguageServerInstallationStatus as Status, Os,
};

use crate::{
    config::Config,
    error::{self, Result},
    server,
};

const SOURCE_FILE: &str = ".vsrocq-bridge-source";
const DEFAULT_REPOSITORY: &str = "George-Miao/zed-vsrocq";

#[derive(Default)]
pub(crate) struct Bridge {
    cached: Option<(String, String)>,
}

impl Bridge {
    pub(crate) fn resolve(
        &mut self,
        id: &LanguageServerId,
        settings: &Config,
        worktree: &zed::Worktree,
    ) -> Result<String> {
        if let Some(path) = settings.binary.as_ref().and_then(|binary| binary.path.as_ref()) {
            return ready(id, server::configured_path(path, worktree)?);
        }
        if let Some(path) = &settings.bridge.path {
            return ready(id, server::configured_path(path, worktree)?);
        }
        if let Some(path) = worktree.which("vsrocq-bridge") {
            return ready(id, path);
        }

        let (os, architecture) = zed::current_platform();
        let file = if matches!(os, Os::Windows) {
            "vsrocq-bridge.exe"
        } else {
            "vsrocq-bridge"
        };
        let path = env::current_dir()
            .context(error::WorkingDirectorySnafu)?
            .join(file)
            .to_string_lossy()
            .into_owned();
        let config = &settings.bridge;
        let repository = config.release_repository.as_deref().unwrap_or(DEFAULT_REPOSITORY);
        let request = if let Some(url) = &config.download_url {
            validate_url(url)?;
            format!("url:{url}")
        } else {
            validate_repository(repository)?;
            format!(
                "github:{repository}:{}",
                config.release_tag.as_deref().unwrap_or("latest")
            )
        };
        if self
            .cached
            .as_ref()
            .is_some_and(|(source, cached)| source == &request && cached == &path)
            && is_file(file)
        {
            return ready(id, path);
        }

        zed::set_language_server_installation_status(id, &Status::CheckingForUpdate);
        let (url, source) = if let Some(url) = &config.download_url {
            (url.clone(), request.clone())
        } else {
            let release = if let Some(tag) = &config.release_tag {
                ensure!(!tag.trim().is_empty(), error::EmptyReleaseTagSnafu);
                error::host(zed::github_release_by_tag_name(repository, tag))
            } else {
                error::host(zed::latest_github_release(repository, GithubReleaseOptions {
                    require_assets: true,
                    pre_release: false,
                }))
            }
            .with_context(|_| error::ReleaseSnafu {
                repository,
                tag: config.release_tag.clone(),
            })?;
            let target = server::target(os, architecture)?;
            let suffix = if matches!(os, Os::Windows) { ".exe" } else { "" };
            // Release assets are raw executables. CI must publish these exact
            // target-based names.
            let name = format!("vsrocq-bridge-{target}{suffix}");
            let asset = release
                .assets
                .into_iter()
                .find(|asset| asset.name == name)
                .with_context(|| error::MissingAssetSnafu {
                    repository,
                    release: release.version.as_str(),
                    asset: name.as_str(),
                })?;
            validate_url(&asset.download_url)?;
            let source = format!("{repository}:{}:{}", release.version, asset.download_url);
            (asset.download_url, source)
        };
        if !is_file(file) || fs::read_to_string(SOURCE_FILE).ok().as_deref() != Some(source.as_str()) {
            zed::set_language_server_installation_status(id, &Status::Downloading);
            let nonce = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos();
            let temporary = format!(".{file}-{nonce}.download");
            let install = (|| {
                error::host(zed::download_file(&url, &temporary, DownloadedFileType::Uncompressed))
                    .context(error::DownloadSnafu { path: &temporary })?;
                if !matches!(os, Os::Windows) {
                    error::host(zed::make_file_executable(&temporary))
                        .context(error::ExecutableSnafu { path: &temporary })?;
                }
                fs::rename(&temporary, file).context(error::InstallSnafu { path: file })?;
                fs::write(SOURCE_FILE, source.as_bytes()).context(error::RecordSourceSnafu)?;
                Ok::<_, error::Error>(())
            })();
            if install.is_err() {
                let _ = fs::remove_file(&temporary);
            }
            install?;
        }
        self.cached = Some((request, path.clone()));
        ready(id, path)
    }
}

fn ready(id: &LanguageServerId, path: String) -> Result<String> {
    zed::set_language_server_installation_status(id, &Status::None);
    Ok(path)
}

fn is_file(path: &str) -> bool {
    fs::metadata(path).is_ok_and(|metadata| metadata.is_file())
}

fn validate_repository(repository: &str) -> Result<()> {
    let mut parts = repository.split('/');
    let valid = |part: &str| {
        !part.is_empty()
            && part != "."
            && part != ".."
            && part
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
    };
    ensure!(
        parts.next().is_some_and(valid) && parts.next().is_some_and(valid) && parts.next().is_none(),
        error::RepositorySnafu { repository },
    );
    Ok(())
}

fn validate_url(url: &str) -> Result<()> {
    ensure!(
        url.starts_with("https://")
            && url.len() > "https://".len()
            && !url.chars().any(char::is_whitespace)
            && !url.contains('#'),
        error::DownloadUrlSnafu,
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requires_explicit_repository_identity() {
        assert!(validate_repository("rocq-prover/vsrocq").is_ok());
        for repository in ["", "repo", "../repo", "owner/../repo", "https://github.com/owner/repo"] {
            assert!(validate_repository(repository).is_err(), "{repository}");
        }
    }

    #[test]
    fn requires_https_downloads() {
        assert!(validate_url("https://example.org/bridge.exe").is_ok());
        for url in [
            "http://example.org/bridge",
            "https://",
            "https://example.org/a b",
            "https://example.org/a#fragment",
        ] {
            assert!(validate_url(url).is_err(), "{url}");
        }
    }
}
