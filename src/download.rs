//! Downloading mod files.
use std::error::Error as StdError;
use std::fmt;
use std::path::Path;
use std::task::Poll;

use bytes::Bytes;
use futures_core::{ready, Stream};
use futures_util::stream::poll_fn;
use futures_util::{future, FutureExt, SinkExt, StreamExt, TryFutureExt};
use log::debug;
use reqwest::{Method, Response, StatusCode};
use tokio_codec::{BytesCodec, FramedWrite};
use tokio_fs::File as AsyncFile;
use tokio_io::BufWriter;

use crate::error::{self, Kind, Result};
use crate::types::mods::{File, Mod};
use crate::Modio;

/// A `Downloader` can be used to stream a mod file or save the file to a local file.
/// Constructed with [`Modio::download`].
pub struct Downloader {
    modio: Modio,
    action: DownloadAction,
}

impl Downloader {
    pub(crate) fn new(modio: Modio, action: DownloadAction) -> Self {
        Self { modio, action }
    }

    /// Save the mod file to a local file.
    ///
    /// # Example
    /// ```no_run
    /// # async fn run() -> Result<(), Box<dyn std::error::Error>> {
    /// #     let modio = modio::Modio::new(modio::Credentials::ApiKey("".to_string()))?;
    /// let action = modio::DownloadAction::Primary {
    ///     game_id: 5,
    ///     mod_id: 19,
    /// };
    ///
    /// modio.download(action).save_to_file("mod.zip").await?;
    /// #     Ok(())
    /// # }
    /// ```
    pub async fn save_to_file<P: AsRef<Path>>(self, file: P) -> Result<()> {
        let out = AsyncFile::create(file).map_err(error::decode).await?;
        let out = BufWriter::with_capacity(512 * 512, out);
        let out = FramedWrite::new(out, BytesCodec::new());
        let out = out.sink_map_err(error::decode);
        self.stream().forward(out).await
    }

    /// `Stream` of bytes of the mod file.
    ///
    /// # Example
    /// ```no_run
    /// use futures_util::{future, TryStreamExt};
    ///
    /// # async fn run() -> Result<(), Box<dyn std::error::Error>> {
    /// #     let modio = modio::Modio::new(modio::Credentials::ApiKey("user-or-game-apikey".to_string()))?;
    /// let action = modio::DownloadAction::Primary {
    ///     game_id: 5,
    ///     mod_id: 19,
    /// };
    ///
    /// modio.download(action)
    ///     .stream()
    ///     .try_for_each(|bytes| {
    ///         println!("Bytes: {:?}", bytes);
    ///         future::ok(())
    ///     })
    ///     .await?;
    /// #     Ok(())
    /// # }
    pub fn stream(self) -> impl Stream<Item = Result<Bytes>> {
        request_file(self.modio, self.action)
            .and_then(|mut res| {
                let st = poll_fn(
                    move |cx| match ready!(Box::pin(res.chunk()).poll_unpin(cx)) {
                        Ok(Some(bytes)) => Poll::Ready(Some(Ok(bytes))),
                        Err(e) => Poll::Ready(Some(Err(error::request(e)))),
                        Ok(None) => Poll::Ready(None),
                    },
                );
                future::ok(st)
            })
            .try_flatten_stream()
    }
}

async fn request_file(modio: Modio, action: DownloadAction) -> Result<Response> {
    let url = match action {
        DownloadAction::Primary { game_id, mod_id } => {
            let modref = modio.mod_(game_id, mod_id);
            let m = modref.get().await?;
            if let Some(file) = m.modfile {
                file.download.binary_url
            } else {
                return Err(error::download_no_primary(game_id, mod_id));
            }
        }
        DownloadAction::FileObj(file) => file.download.binary_url,
        DownloadAction::File {
            game_id,
            mod_id,
            file_id,
        } => {
            let fileref = modio.mod_(game_id, mod_id).file(file_id);
            let file = fileref
                .get()
                .map_err(|e| match e.kind() {
                    Kind::Status(StatusCode::NOT_FOUND) => {
                        error::download_file_not_found(game_id, mod_id, file_id)
                    }
                    _ => e,
                })
                .await?;
            file.download.binary_url
        }
        DownloadAction::Version {
            game_id,
            mod_id,
            version,
            policy,
        } => {
            use crate::files::filters::{DateAdded, Version};
            use crate::filter::prelude::*;
            use ResolvePolicy::*;

            let filter = Version::eq(version.clone())
                .order_by(DateAdded::desc())
                .limit(2);

            let files = modio.mod_(game_id, mod_id).files();
            let mut list = files.list(filter).await?;

            let (file, error) = match (list.count, policy) {
                (0, _) => (
                    None,
                    Some(error::download_version_not_found(game_id, mod_id, version)),
                ),
                (1, _) => (list.shift(), None),
                (_, Latest) => (list.shift(), None),
                (_, Fail) => (
                    None,
                    Some(error::download_multiple_files(game_id, mod_id, version)),
                ),
            };

            if let Some(file) = file {
                file.download.binary_url
            } else {
                return Err(error.expect("bug in previous match!"));
            }
        }
    };

    debug!("downloading file: {}", url);
    modio
        .client
        .request(Method::GET, url)
        .send()
        .map_err(error::builder_or_request)
        .await?
        .error_for_status()
        .map_err(error::request)
}

/// Defines the action that is performed for [`Modio::download`].
#[derive(Debug)]
pub enum DownloadAction {
    /// Download the primary modfile of a mod.
    Primary { game_id: u32, mod_id: u32 },
    /// Download a specific modfile of a mod.
    File {
        game_id: u32,
        mod_id: u32,
        file_id: u32,
    },
    /// Download a specific modfile.
    FileObj(Box<File>),
    /// Download a specific version of a mod.
    Version {
        game_id: u32,
        mod_id: u32,
        version: String,
        policy: ResolvePolicy,
    },
}

/// Defines the policy for `DownloadAction::Version` when multiple files are found.
#[derive(Debug)]
pub enum ResolvePolicy {
    /// Download the latest file.
    Latest,
    /// Return with [`Error::MultipleFilesFound`] as source error.
    Fail,
}

/// The Errors that may occur when using [`Modio::download`](../struct.Modio.html#method.download).
#[derive(Debug)]
pub enum Error {
    /// The mod has no primary file.
    NoPrimaryFile { game_id: u32, mod_id: u32 },
    /// The specific file of a mod was not found.
    FileNotFound {
        game_id: u32,
        mod_id: u32,
        file_id: u32,
    },
    /// Multiple files for a given version were found and the policy was set to
    /// [`ResolvePolicy::Fail`](../download/enum.ResolvePolicy.html#variant.Fail).
    MultipleFilesFound {
        game_id: u32,
        mod_id: u32,
        version: String,
    },
    /// No file for a given version was found.
    VersionNotFound {
        game_id: u32,
        mod_id: u32,
        version: String,
    },
}

impl StdError for Error {}

impl fmt::Display for Error {
    fn fmt(&self, fmt: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::FileNotFound {
                game_id,
                mod_id,
                file_id,
            } => write!(
                fmt,
                "Mod {{id: {1}, game_id: {0}}}: File {{ id: {2} }} not found.",
                game_id, mod_id, file_id,
            ),
            Error::MultipleFilesFound {
                game_id,
                mod_id,
                version,
            } => write!(
                fmt,
                "Mod {{id: {1}, game_id: {0}}}: Multiple files found for version '{2}'.",
                game_id, mod_id, version,
            ),
            Error::NoPrimaryFile { game_id, mod_id } => write!(
                fmt,
                "Mod {{id: {1}, game_id: {0}}} Mod has no primary file.",
                game_id, mod_id,
            ),
            Error::VersionNotFound {
                game_id,
                mod_id,
                version,
            } => write!(
                fmt,
                "Mod {{id: {1}, game_id: {0}}}: No file with version '{2}' found.",
                game_id, mod_id, version,
            ),
        }
    }
}

/// Convert `Mod` to [`DownloadAction::File`](enum.DownloadAction.html#variant.File) or
/// [`DownloadAction::Primary`](enum.DownloadAction.html#variant.Primary) if `Mod::modfile` is `None`
impl From<Mod> for DownloadAction {
    fn from(m: Mod) -> DownloadAction {
        if let Some(file) = m.modfile {
            DownloadAction::from(file)
        } else {
            DownloadAction::Primary {
                game_id: m.game_id,
                mod_id: m.id,
            }
        }
    }
}

/// Convert `File` to [`DownloadAction::FileObj`](enum.DownloadAction.html#variant.FileObj)
impl From<File> for DownloadAction {
    fn from(file: File) -> DownloadAction {
        DownloadAction::FileObj(Box::new(file))
    }
}

/// Convert `(u32, u32)` to [`DownloadAction::Primary`](enum.DownloadAction.html#variant.Primary)
impl From<(u32, u32)> for DownloadAction {
    fn from((game_id, mod_id): (u32, u32)) -> DownloadAction {
        DownloadAction::Primary { game_id, mod_id }
    }
}

/// Convert `(u32, u32, u32)` to
/// [`DownloadAction::File`](enum.DownloadAction.html#variant.File)
impl From<(u32, u32, u32)> for DownloadAction {
    fn from((game_id, mod_id, file_id): (u32, u32, u32)) -> DownloadAction {
        DownloadAction::File {
            game_id,
            mod_id,
            file_id,
        }
    }
}

/// Convert `(u32, u32, String)` to [`DownloadAction::Version`](enum.DownloadAction.html#variant.Version)
/// with resolve policy set to `ResolvePolicy::Latest`
impl From<(u32, u32, String)> for DownloadAction {
    fn from((game_id, mod_id, version): (u32, u32, String)) -> DownloadAction {
        DownloadAction::Version {
            game_id,
            mod_id,
            version,
            policy: ResolvePolicy::Latest,
        }
    }
}

/// Convert `(u32, u32, &'a str)` to [`DownloadAction::Version`](enum.DownloadAction.html#variant.Version)
/// with resolve policy set to `ResolvePolicy::Latest`
impl<'a> From<(u32, u32, &'a str)> for DownloadAction {
    fn from((game_id, mod_id, version): (u32, u32, &'a str)) -> DownloadAction {
        DownloadAction::Version {
            game_id,
            mod_id,
            version: version.to_string(),
            policy: ResolvePolicy::Latest,
        }
    }
}
