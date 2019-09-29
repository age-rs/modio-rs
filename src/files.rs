//! Modfile interface
use std::ffi::OsStr;
use std::marker::Unpin;
use std::path::Path;

use mime::APPLICATION_OCTET_STREAM;
use tokio_io::AsyncRead;
use url::form_urlencoded;

use crate::multipart::FileSource;
use crate::prelude::*;

pub use crate::types::mods::{Download, File, FileHash};

/// Interface for the modfiles the authenticated user uploaded.
pub struct MyFiles {
    modio: Modio,
}

impl MyFiles {
    pub(crate) fn new(modio: Modio) -> Self {
        Self { modio }
    }

    /// Return all modfiles the authenticated user uploaded. [required: token]
    ///
    /// See [Filters and sorting](filters/index.html).
    pub async fn list(self, filter: Filter) -> Result<List<File>> {
        self.modio
            .request(Route::UserFiles)
            .query(filter.to_query_string())
            .send()
            .await
    }

    /// Provides a stream over all modfiles the authenticated user uploaded. [required: token]
    ///
    /// See [Filters and sorting](filters/index.html).
    pub fn iter<'a>(self, filter: Filter) -> Iter<'a, File> {
        self.modio.stream(Route::UserFiles, filter)
    }
}

/// Interface for the modfiles of a mod.
pub struct Files {
    modio: Modio,
    game: u32,
    mod_id: u32,
}

impl Files {
    pub(crate) fn new(modio: Modio, game: u32, mod_id: u32) -> Self {
        Self {
            modio,
            game,
            mod_id,
        }
    }

    /// Return all files that are published for a mod this `Files` refers to.
    ///
    /// See [Filters and sorting](filters/index.html).
    pub async fn list(self, filter: Filter) -> Result<List<File>> {
        let route = Route::GetFiles {
            game_id: self.game,
            mod_id: self.mod_id,
        };
        self.modio
            .request(route)
            .query(filter.to_query_string())
            .send()
            .await
    }

    /// Provides a stream over all files that are published for a mod this `Files` refers to.
    ///
    /// See [Filters and sorting](filters/index.html).
    pub fn iter<'a>(self, filter: Filter) -> Iter<'a, File> {
        let route = Route::GetFiles {
            game_id: self.game,
            mod_id: self.mod_id,
        };
        self.modio.stream(route, filter)
    }

    /// Return a reference to a file.
    pub fn get(&self, id: u32) -> FileRef {
        FileRef::new(self.modio.clone(), self.game, self.mod_id, id)
    }

    /// Add a file for a mod that this `Files` refers to. [required: token]
    pub async fn add(self, options: AddFileOptions) -> Result<File> {
        let route = Route::AddFile {
            game_id: self.game,
            mod_id: self.mod_id,
        };
        self.modio
            .request(route)
            .body(Form::from(options))
            .send()
            .await
    }
}

/// Reference interface of a modfile.
pub struct FileRef {
    modio: Modio,
    game: u32,
    mod_id: u32,
    id: u32,
}

impl FileRef {
    pub(crate) fn new(modio: Modio, game: u32, mod_id: u32, id: u32) -> Self {
        Self {
            modio,
            game,
            mod_id,
            id,
        }
    }

    /// Get a reference to the Modio modfile object that this `FileRef` refers to.
    pub async fn get(self) -> Result<File> {
        let route = Route::GetFile {
            game_id: self.game,
            mod_id: self.mod_id,
            file_id: self.id,
        };
        self.modio.request(route).send().await
    }

    /// Edit details of a modfile. [required: token]
    pub async fn edit(self, options: EditFileOptions) -> Result<EntityResult<File>> {
        let route = Route::EditFile {
            game_id: self.game,
            mod_id: self.mod_id,
            file_id: self.id,
        };
        self.modio
            .request(route)
            .body(options.to_query_string())
            .send()
            .await
    }

    /// Delete a modfile. [required: token]
    pub async fn delete(self) -> Result<()> {
        let route = Route::DeleteFile {
            game_id: self.game,
            mod_id: self.mod_id,
            file_id: self.id,
        };
        self.modio.request(route).send().await
    }
}

/// Modfile filters and sorting.
///
/// # Filters
/// - Fulltext
/// - Id
/// - ModId
/// - DateAdded
/// - DateScanned
/// - VirusStatus
/// - VirusPositive
/// - Filesize
/// - Filehash
/// - Filename
/// - Version
/// - Changelog
///
/// # Sorting
/// - Id
/// - ModId
/// - DateAdded
/// - Version
///
/// See [modio docs](https://docs.mod.io/#get-all-modfiles) for more information.
///
/// By default this returns up to `100` items. You can limit the result by using `limit` and
/// `offset`.
///
/// # Example
/// ```
/// use modio::filter::prelude::*;
/// use modio::files::filters::Id;
///
/// let filter = Id::_in(vec![1, 2]).order_by(Id::desc());
/// ```
#[rustfmt::skip]
pub mod filters {
    #[doc(inline)]
    pub use crate::filter::prelude::Fulltext;
    #[doc(inline)]
    pub use crate::filter::prelude::Id;
    #[doc(inline)]
    pub use crate::filter::prelude::ModId;
    #[doc(inline)]
    pub use crate::filter::prelude::DateAdded;

    filter!(DateScanned, DATE_SCANNED, "date_scanned", Eq, NotEq, In, Cmp);
    filter!(VirusStatus, VIRUS_STATUS, "virus_status", Eq, NotEq, In, Cmp);
    filter!(VirusPositive, VIRUS_POSITIVE, "virus_positive", Eq, NotEq, In, Cmp);
    filter!(Filesize, FILESIZE, "filesize", Eq, NotEq, In, Cmp, OrderBy);
    filter!(Filehash, FILEHASH, "filehash", Eq, NotEq, In, Like);
    filter!(Filename, FILENAME, "filename", Eq, NotEq, In, Like);
    filter!(Version, VERSION, "version", Eq, NotEq, In, Like, OrderBy);
    filter!(Changelog, CHANGELOG, "changelog", Eq, NotEq, In, Like);
}

pub struct AddFileOptions {
    source: FileSource,
    version: Option<String>,
    changelog: Option<String>,
    active: Option<bool>,
    filehash: Option<String>,
    metadata_blob: Option<String>,
}

impl AddFileOptions {
    pub fn with_read<R, S>(inner: R, filename: S) -> AddFileOptions
    where
        R: AsyncRead + Send + Sync + Unpin + 'static,
        S: Into<String>,
    {
        AddFileOptions {
            source: FileSource::new_from_read(inner, filename.into(), APPLICATION_OCTET_STREAM),
            version: None,
            changelog: None,
            active: None,
            filehash: None,
            metadata_blob: None,
        }
    }

    pub fn with_file<P: AsRef<Path>>(file: P) -> AddFileOptions {
        let file = file.as_ref();
        let filename = file
            .file_name()
            .and_then(OsStr::to_str)
            .map_or_else(String::new, ToString::to_string);

        Self::with_file_name(file, filename)
    }

    pub fn with_file_name<P, S>(file: P, filename: S) -> AddFileOptions
    where
        P: AsRef<Path>,
        S: Into<String>,
    {
        let file = file.as_ref();

        AddFileOptions {
            source: FileSource::new_from_file(file, filename.into(), APPLICATION_OCTET_STREAM),
            version: None,
            changelog: None,
            active: None,
            filehash: None,
            metadata_blob: None,
        }
    }

    option!(version);
    option!(changelog);
    option!(active: bool);
    option!(filehash);
    option!(metadata_blob);
}

#[doc(hidden)]
impl From<AddFileOptions> for Form {
    fn from(opts: AddFileOptions) -> Form {
        let mut form = Form::new();
        if let Some(version) = opts.version {
            form = form.text("version", version);
        }
        if let Some(changelog) = opts.changelog {
            form = form.text("changelog", changelog);
        }
        if let Some(active) = opts.active {
            form = form.text("active", active.to_string());
        }
        if let Some(filehash) = opts.filehash {
            form = form.text("filehash", filehash);
        }
        if let Some(metadata_blob) = opts.metadata_blob {
            form = form.text("metadata_blob", metadata_blob);
        }
        form.part("filedata", opts.source.into())
    }
}

#[derive(Default)]
pub struct EditFileOptions {
    params: std::collections::BTreeMap<&'static str, String>,
}

impl EditFileOptions {
    option!(version >> "version");
    option!(changelog >> "changelog");
    option!(active: bool >> "active");
    option!(metadata_blob >> "metadata_blob");
}

impl QueryString for EditFileOptions {
    fn to_query_string(&self) -> String {
        form_urlencoded::Serializer::new(String::new())
            .extend_pairs(&self.params)
            .finish()
    }
}
