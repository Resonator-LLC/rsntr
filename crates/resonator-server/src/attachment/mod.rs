//! Attachments: files on disk, their metadata in SQLite.
//!
//! Derived from `AttachmentService`, `AttachmentStore`,
//! `FileAttachmentStore` and `AttachmentUtil` in
//! `org.waveprotocol.box.server` and `AttachmentId` in
//! `org.waveprotocol.wave.media.model` of Apache Wave, licensed under the
//! Apache License, Version 2.0.
//!
//! An attachment has an id chosen by the client (`<domain>/<id>` or just
//! `<id>`), belongs to one wavelet, and can be read by whoever can read
//! that wavelet. Checking that is the caller's business ([`crate::http`]
//! does it for the endpoints); this module stores and serves.
//!
//! # Storage
//!
//! Files live under the attachments directory: `files/<xx>/<hash>` for the
//! bytes and `files/<xx>/<hash>.thumb` for the thumbnail of an image, where
//! `<hash>` is the SHA-256 of the serialised id in hexadecimal and `<xx>`
//! its first two digits. No part of a path comes from the id itself, so an
//! id cannot name a file outside the directory. An upload is written to
//! `tmp/`, synced, renamed into place, and only then is its metadata row
//! committed; a file without a row is an orphan, removed by
//! [`AttachmentService::sweep`]. An attachment is never overwritten.
//!
//! Differences from the original: metadata is a database row rather than a
//! `.meta` file; an attachment without metadata does not exist (the
//! original builds metadata on first download, trusting the wave reference
//! in the request); storing an id twice is refused rather than an error
//! after half the work.

pub mod mime;
pub mod thumbnail;

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime};

use futures_util::future::BoxFuture;

use resonator_model::id::constants::CONVERSATION_ROOT_WAVELET;
use resonator_model::id::{ParticipantId, WaveRef, WaveletId, WaveletName};
use resonator_model::proto::attachment as proto;
use rusqlite::{OptionalExtension, params};
use sha2::{Digest as _, Sha256};
use thiserror::Error;
use tokio::io::AsyncWriteExt;
use tokio::sync::Semaphore;

use self::thumbnail::{DecodeLimits, PATTERN_HEIGHT, PATTERN_WIDTH, Thumbnail};
use crate::clock::{Clock, SystemClock};
use crate::config::Config;
use crate::storage::{Database, StorageError};

/// Path prefix of attachment downloads (`AttachmentServlet.ATTACHMENT_URL`).
pub const ATTACHMENT_URL: &str = "/attachment";
/// Path prefix of thumbnails (`AttachmentServlet.THUMBNAIL_URL`).
pub const THUMBNAIL_URL: &str = "/thumbnail";

/// Longest serialised attachment id accepted, in bytes.
pub const MAX_ID_LENGTH: usize = 256;
/// Longest file name kept, in characters.
pub const MAX_FILE_NAME_LENGTH: usize = 255;

/// Thumbnails being made at a time; decoding an image can take a lot of
/// memory.
const THUMBNAIL_JOBS: usize = 2;

/// An attachment id (`AttachmentId`): a domain and an id, or just an id
/// for attachments from before ids had domains.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct AttachmentId {
    domain: String,
    id: String,
}

/// A string that is not an attachment id.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("invalid attachment id '{0}'")]
pub struct InvalidAttachmentId(pub String);

impl AttachmentId {
    /// `AttachmentId.deserialise`: `<domain>/<id>` or `<id>`. Unlike the
    /// original, an id must not be empty, overlong or contain control
    /// characters.
    pub fn deserialise(serialised: &str) -> Result<Self, InvalidAttachmentId> {
        let invalid = || InvalidAttachmentId(serialised.chars().take(64).collect());
        if serialised.len() > MAX_ID_LENGTH || serialised.chars().any(char::is_control) {
            return Err(invalid());
        }
        let (domain, id) = match serialised.split_once('/') {
            Some((domain, id)) => (domain, id),
            None => ("", serialised),
        };
        if id.is_empty() || id.contains('/') || (domain.is_empty() && serialised.contains('/')) {
            return Err(invalid());
        }
        Ok(AttachmentId {
            domain: domain.to_owned(),
            id: id.to_owned(),
        })
    }

    pub fn domain(&self) -> &str {
        &self.domain
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    /// The domain the id names or, for an id that names none (one made by
    /// a client that did not know its server federates), the domain
    /// hosting the wavelet: the first place to look for it. Such an
    /// attachment may also be on the server of any participant.
    pub fn origin_domain<'a>(&'a self, wavelet: &'a WaveletName) -> &'a str {
        if self.domain.is_empty() {
            wavelet.wavelet_id.domain()
        } else {
            &self.domain
        }
    }

    /// `AttachmentId.serialise`.
    pub fn serialise(&self) -> String {
        if self.domain.is_empty() {
            self.id.clone()
        } else {
            format!("{}/{}", self.domain, self.id)
        }
    }

    /// The name of the attachment's file: the SHA-256 of the serialised
    /// id, in hexadecimal.
    fn file_stem(&self) -> String {
        Sha256::digest(self.serialise().as_bytes())
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect()
    }
}

impl fmt::Display for AttachmentId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.serialise())
    }
}

/// Width and height of an image (`ImageMetadata`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ImageSize {
    pub width: u32,
    pub height: u32,
}

/// What is known about an attachment (`AttachmentMetadata`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttachmentMetadata {
    pub id: AttachmentId,
    /// The wavelet the attachment belongs to, as a wave reference path
    /// (`example.com/w+abc/~/conv+root`).
    pub wave_ref: String,
    pub file_name: String,
    /// From the file name; see [`mime::mime_type_for`].
    pub mime_type: String,
    pub size: u64,
    /// Address of the uploader; empty if unknown.
    pub creator: String,
    /// Set if the attachment is an image with a stored thumbnail.
    pub image: Option<ImageSize>,
    /// The size of the thumbnail: of the stored one for an image, of the
    /// stand-in picture otherwise.
    pub thumbnail: Option<ImageSize>,
    /// Whether a scanner flagged the file; `None` if none looked. Nothing
    /// in this server sets it.
    pub malware: Option<bool>,
    /// SHA-256 of the bytes.
    pub sha256: [u8; 32],
    /// Milliseconds since the epoch.
    pub created_at: i64,
    /// For a copy of another server's attachment: that server's domain
    /// (the domain of the id). `None` for attachments uploaded here.
    pub origin: Option<String>,
    /// A copy whose files were dropped from the cache; the row stays and
    /// pins the hash. Fetched again when asked for.
    pub evicted: bool,
}

impl AttachmentMetadata {
    /// The wavelet the attachment belongs to
    /// (`AttachmentUtil.waveRef2WaveletName`).
    pub fn wavelet_name(&self) -> Option<WaveletName> {
        wave_ref_to_wavelet_name(&self.wave_ref)
    }

    /// `/attachment/<id>`.
    pub fn attachment_url(&self) -> String {
        format!("{ATTACHMENT_URL}/{}", self.id)
    }

    /// `/thumbnail/<id>`.
    pub fn thumbnail_url(&self) -> String {
        format!("{THUMBNAIL_URL}/{}", self.id)
    }

    /// The entity tag of the bytes.
    pub fn etag(&self) -> String {
        let hex: String = self.sha256[..16]
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        format!("\"{hex}\"")
    }

    /// The message of `attachment.proto`.
    pub fn to_proto(&self) -> proto::AttachmentMetadata {
        let size = |size: ImageSize| proto::ImageMetadata {
            width: Some(i32::try_from(size.width).unwrap_or(i32::MAX)),
            height: Some(i32::try_from(size.height).unwrap_or(i32::MAX)),
        };
        proto::AttachmentMetadata {
            attachment_id: Some(self.id.serialise()),
            wave_ref: Some(self.wave_ref.clone()),
            file_name: Some(self.file_name.clone()),
            mime_type: Some(self.mime_type.clone()),
            size: Some(i64::try_from(self.size).unwrap_or(i64::MAX)),
            creator: Some(self.creator.clone()),
            attachment_url: Some(self.attachment_url()),
            thumbnail_url: Some(self.thumbnail_url()),
            image_metadata: self.image.map(size),
            thumbnail_metadata: self.thumbnail.map(size),
            malware: self.malware,
        }
    }
}

/// The wavelet a wave reference path names; the root conversation wavelet
/// if it names only a wave (`AttachmentUtil.waveRef2WaveletName`).
pub fn wave_ref_to_wavelet_name(wave_ref: &str) -> Option<WaveletName> {
    let wave_ref = WaveRef::decode_from_path(wave_ref).ok()?;
    let wavelet_id = match wave_ref.wavelet_id() {
        Some(wavelet_id) => wavelet_id.clone(),
        None => WaveletId::of(wave_ref.wave_id().domain(), CONVERSATION_ROOT_WAVELET).ok()?,
    };
    Some(WaveletName::of(wave_ref.wave_id().clone(), wavelet_id))
}

/// The wave reference path of a wavelet
/// (`AttachmentService.waveletName2WaveRef`).
pub fn wavelet_name_to_wave_ref(wavelet_name: &WaveletName) -> String {
    WaveRef::of_wavelet(
        wavelet_name.wave_id.clone(),
        wavelet_name.wavelet_id.clone(),
    )
    .encode_to_uri_path_segment()
}

/// The last component of a path as a client may send it, without control
/// characters, at most [`MAX_FILE_NAME_LENGTH`] characters
/// (`FilenameUtils.getName` in the original's upload).
pub fn sanitize_file_name(name: &str) -> String {
    let base = name.rsplit(['/', '\\']).next().unwrap_or("");
    let cleaned: String = base
        .chars()
        .filter(|c| !c.is_control())
        .take(MAX_FILE_NAME_LENGTH)
        .collect();
    cleaned.trim().to_owned()
}

/// Failure of an attachment operation.
#[derive(Debug, Error)]
pub enum AttachmentError {
    #[error("attachment {0} already exists")]
    AlreadyExists(AttachmentId),
    #[error("attachment is larger than {max} bytes")]
    TooLarge { max: u64 },
    #[error("the uploader's attachment quota of {quota} bytes is used up")]
    QuotaExceeded { quota: u64 },
    #[error("attachment i/o error: {0}")]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Storage(#[from] StorageError),
}

/// Why an attachment of another server could not be had.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum FetchError {
    /// The origin has no such attachment, or not for that wavelet, or not
    /// for this server.
    #[error("the origin has no such attachment for this server")]
    NotFound,
    /// The origin cannot be reached (or answers with a failure).
    #[error("unavailable from {domain}: {message}")]
    Unavailable { domain: String, message: String },
    /// What the origin sent does not match what it said it would send, or
    /// what this server pinned for the id.
    #[error("the origin's copy was rejected: {0}")]
    Rejected(String),
    /// Failure on this side.
    #[error("cannot store the fetched attachment: {0}")]
    Local(String),
}

/// What the origin of an attachment says about it before sending the
/// bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteAttachment {
    pub file_name: String,
    pub size: u64,
    /// Address of the uploader.
    pub creator: String,
    pub sha256: [u8; 32],
    pub created_at: i64,
    /// The domain of the server it was fetched from.
    pub origin: String,
}

/// Fetches attachments from the servers they were uploaded to
/// (federation's `fetch-attachment`). Implemented by the federation crate.
pub trait AttachmentOrigin: Send + Sync {
    /// Asks the origin (the domain of `id`; for an id without one, the
    /// wavelet's host and the servers of its participants in turn) for the
    /// attachment `id` of `wavelet` and writes its bytes into `upload`. The caller checks
    /// sizes and hashes once this returns.
    fn fetch<'a>(
        &'a self,
        id: &'a AttachmentId,
        wavelet: &'a WaveletName,
        upload: &'a mut Upload,
    ) -> BoxFuture<'a, Result<RemoteAttachment, FetchError>>;
}

/// Told of every attachment uploaded here (not of fetched copies), right
/// after it is stored. Must not block: the upload's answer waits for it.
/// Installed by federation (person mode: copies to the person's other
/// devices).
pub trait UploadListener: Send + Sync {
    fn uploaded(&self, metadata: &AttachmentMetadata);
}

/// How long an origin that could not be reached is not asked again.
const UNAVAILABLE_BACKOFF: Duration = Duration::from_secs(15);

/// Settings of an [`AttachmentService`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttachmentSettings {
    /// The attachments directory.
    pub directory: PathBuf,
    /// Largest attachment accepted, in bytes.
    pub max_size: u64,
    /// Most bytes of attachments one user may have uploaded; 0 for no
    /// limit.
    pub user_quota: u64,
    pub decode_limits: DecodeLimits,
    /// This server's wave domain: attachments with another domain in their
    /// id belong to other servers.
    pub domain: String,
    /// Most bytes of other servers' attachments kept here.
    pub cache_budget: u64,
    /// Person mode: other devices share this server's domain, so an
    /// attachment of this domain that is not here may be on one of them
    /// (and is fetched from there).
    pub shared_domain: bool,
}

impl AttachmentSettings {
    pub fn new(directory: impl Into<PathBuf>) -> Self {
        let config = Config::default();
        AttachmentSettings {
            directory: directory.into(),
            max_size: config.attachment_max_size,
            user_quota: 0,
            decode_limits: DecodeLimits::default(),
            domain: String::new(),
            cache_budget: config.federation.attachment_cache_bytes(),
            shared_domain: false,
        }
    }

    pub fn from_config(config: &Config) -> Self {
        AttachmentSettings {
            directory: config.attachments_directory(),
            max_size: config.attachment_max_size,
            user_quota: config.attachment_user_quota,
            decode_limits: DecodeLimits::default(),
            domain: config.domain.clone(),
            cache_budget: config.federation.attachment_cache_bytes(),
            shared_domain: config.federation.enabled && config.person.enabled,
        }
    }
}

/// What [`AttachmentService::sweep`] removed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SweepReport {
    /// Abandoned uploads.
    pub temporary_files: usize,
    /// Files without a metadata row.
    pub orphans: usize,
}

/// A fetch of another server's attachment, shared by everyone asking for
/// it while it runs.
type Fetch = Arc<tokio::sync::OnceCell<Result<AttachmentMetadata, FetchError>>>;

struct Inner {
    db: Database,
    settings: AttachmentSettings,
    clock: Arc<dyn Clock>,
    /// Ids being stored right now.
    storing: Mutex<HashSet<String>>,
    thumbnails: Semaphore,
    /// Where other servers' attachments come from; unset without
    /// federation.
    origin: OnceLock<Arc<dyn AttachmentOrigin>>,
    /// Told of uploads; unset without federation.
    listener: Mutex<Option<Arc<dyn UploadListener>>>,
    /// Fetches running, by id and wavelet.
    fetches: Mutex<HashMap<(String, String), Fetch>>,
    /// Origins found unreachable, until when they are not asked again.
    unavailable: Mutex<HashMap<String, (Instant, String)>>,
    /// Fetches from other servers started.
    remote_fetches: std::sync::atomic::AtomicU64,
}

/// The attachment store and service; cheap to clone.
#[derive(Clone)]
pub struct AttachmentService {
    inner: Arc<Inner>,
}

impl fmt::Debug for AttachmentService {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AttachmentService")
            .field("directory", &self.inner.settings.directory)
            .finish()
    }
}

/// An upload in progress: bytes go to a temporary file, which is removed
/// unless the upload is finished with [`AttachmentService::finish_upload`].
pub struct Upload {
    file: Option<tokio::fs::File>,
    path: PathBuf,
    size: u64,
    max_size: u64,
    /// The uploader's quota and what was left of it when the upload began.
    quota: Option<(u64, u64)>,
    hasher: Sha256,
}

impl Upload {
    /// Appends bytes. Fails once the attachment is larger than allowed.
    pub async fn write(&mut self, bytes: &[u8]) -> Result<(), AttachmentError> {
        self.size = self.size.saturating_add(bytes.len() as u64);
        if self.size > self.max_size {
            return Err(AttachmentError::TooLarge { max: self.max_size });
        }
        if let Some((quota, left)) = self.quota
            && self.size > left
        {
            return Err(AttachmentError::QuotaExceeded { quota });
        }
        self.hasher.update(bytes);
        match &mut self.file {
            Some(file) => Ok(file.write_all(bytes).await?),
            None => Err(AttachmentError::Io(std::io::Error::other(
                "upload already finished",
            ))),
        }
    }

    /// Bytes written so far.
    pub fn size(&self) -> u64 {
        self.size
    }

    /// The most bytes this upload takes.
    pub fn max_size(&self) -> u64 {
        self.max_size
    }
}

impl Drop for Upload {
    fn drop(&mut self) {
        if self.file.take().is_some() {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

/// Removes the id from the set of ids being stored.
struct Storing<'a> {
    inner: &'a Inner,
    id: String,
}

impl Drop for Storing<'_> {
    fn drop(&mut self) {
        lock(&self.inner.storing).remove(&self.id);
    }
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|e| e.into_inner())
}

const COLUMNS: &str = "id, wave_ref, file_name, mime_type, size, creator, image_width, \
                       image_height, thumbnail_width, thumbnail_height, malware, sha256, created_at, \
                       origin, evicted";

fn read_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Option<AttachmentMetadata>> {
    let id: String = row.get(0)?;
    let Ok(id) = AttachmentId::deserialise(&id) else {
        return Ok(None);
    };
    let size = |width: Option<u32>, height: Option<u32>| match (width, height) {
        (Some(width), Some(height)) => Some(ImageSize { width, height }),
        _ => None,
    };
    let sha256: Vec<u8> = row.get(11)?;
    Ok(Some(AttachmentMetadata {
        id,
        wave_ref: row.get(1)?,
        file_name: row.get(2)?,
        mime_type: row.get(3)?,
        size: row.get::<_, i64>(4)?.max(0) as u64,
        creator: row.get(5)?,
        image: size(row.get(6)?, row.get(7)?),
        thumbnail: size(row.get(8)?, row.get(9)?),
        malware: row.get(10)?,
        sha256: sha256.try_into().unwrap_or([0; 32]),
        created_at: row.get(12)?,
        origin: row.get(13)?,
        evicted: row.get::<_, i64>(14)? != 0,
    }))
}

async fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> std::io::Result<T> + Send + 'static,
) -> std::io::Result<T> {
    tokio::task::spawn_blocking(work)
        .await
        .map_err(std::io::Error::other)?
}

fn sync_directory(path: &Path) -> std::io::Result<()> {
    std::fs::File::open(path)?.sync_all()
}

impl AttachmentService {
    /// Opens the store, creating its directories, and removes the uploads
    /// a previous run left unfinished. Blocking; call it at start-up.
    pub fn open(db: &Database, settings: AttachmentSettings) -> std::io::Result<Self> {
        Self::open_with_clock(db, settings, Arc::new(SystemClock))
    }

    pub fn open_with_clock(
        db: &Database,
        settings: AttachmentSettings,
        clock: Arc<dyn Clock>,
    ) -> std::io::Result<Self> {
        let describe = |error: std::io::Error| {
            std::io::Error::new(
                error.kind(),
                format!(
                    "cannot use attachments directory {}: {error}",
                    settings.directory.display()
                ),
            )
        };
        std::fs::create_dir_all(settings.directory.join("files")).map_err(describe)?;
        let tmp = settings.directory.join("tmp");
        std::fs::create_dir_all(&tmp).map_err(describe)?;
        // Nothing is in flight yet: whatever is in tmp is abandoned.
        for entry in std::fs::read_dir(&tmp).map_err(describe)?.flatten() {
            let _ = std::fs::remove_file(entry.path());
        }
        Ok(AttachmentService {
            inner: Arc::new(Inner {
                db: db.clone(),
                settings,
                clock,
                storing: Mutex::new(HashSet::new()),
                thumbnails: Semaphore::new(THUMBNAIL_JOBS),
                origin: OnceLock::new(),
                listener: Mutex::new(None),
                fetches: Mutex::new(HashMap::new()),
                unavailable: Mutex::new(HashMap::new()),
                remote_fetches: std::sync::atomic::AtomicU64::new(0),
            }),
        })
    }

    pub fn settings(&self) -> &AttachmentSettings {
        &self.inner.settings
    }

    fn file_path(&self, id: &AttachmentId) -> PathBuf {
        let stem = id.file_stem();
        self.inner
            .settings
            .directory
            .join("files")
            .join(&stem[..2])
            .join(stem)
    }

    fn thumbnail_path(&self, id: &AttachmentId) -> PathBuf {
        let mut path = self.file_path(id).into_os_string();
        path.push(".thumb");
        PathBuf::from(path)
    }

    fn temporary_path(&self) -> std::io::Result<PathBuf> {
        let mut random = [0u8; 16];
        getrandom::fill(&mut random).map_err(std::io::Error::other)?;
        let name: String = random.iter().map(|byte| format!("{byte:02x}")).collect();
        Ok(self.inner.settings.directory.join("tmp").join(name))
    }

    /// The metadata of an attachment (`AttachmentService.getMetadata`).
    pub async fn metadata(
        &self,
        id: &AttachmentId,
    ) -> Result<Option<AttachmentMetadata>, AttachmentError> {
        let id = id.serialise();
        Ok(self
            .inner
            .db
            .read(move |conn| {
                Ok(conn
                    .prepare_cached(&format!("SELECT {COLUMNS} FROM attachments WHERE id = ?1"))?
                    .query_row([&id], read_row)
                    .optional()?
                    .flatten())
            })
            .await?)
    }

    /// The attachments of a wavelet, by id.
    pub async fn list_for_wavelet(
        &self,
        wavelet_name: &WaveletName,
    ) -> Result<Vec<AttachmentMetadata>, AttachmentError> {
        let wave_ref = wavelet_name_to_wave_ref(wavelet_name);
        Ok(self
            .inner
            .db
            .read(move |conn| {
                let mut statement = conn.prepare_cached(&format!(
                    "SELECT {COLUMNS} FROM attachments WHERE wave_ref = ?1 ORDER BY id"
                ))?;
                let rows = statement.query_map([&wave_ref], read_row)?;
                let mut list = Vec::new();
                for row in rows {
                    list.extend(row?);
                }
                Ok(list)
            })
            .await?)
    }

    /// The bytes of the attachments `creator` has uploaded.
    pub async fn usage_of(&self, creator: &ParticipantId) -> Result<u64, AttachmentError> {
        let creator = creator.address().to_owned();
        Ok(self
            .inner
            .db
            .read(move |conn| {
                let total: i64 = conn
                    .prepare_cached(
                        "SELECT COALESCE(SUM(size), 0) FROM attachments \
                         WHERE creator = ?1 AND origin IS NULL",
                    )?
                    .query_row([creator], |row| row.get(0))?;
                Ok(total.max(0) as u64)
            })
            .await?)
    }

    /// How many more bytes `creator` may upload: `None` if there is no
    /// quota, otherwise the quota and what is left of it.
    pub async fn quota_left(
        &self,
        creator: &ParticipantId,
    ) -> Result<Option<(u64, u64)>, AttachmentError> {
        let quota = self.inner.settings.user_quota;
        if quota == 0 {
            return Ok(None);
        }
        let used = self.usage_of(creator).await?;
        Ok(Some((quota, quota.saturating_sub(used))))
    }

    /// Starts an upload by `creator`, which fails as soon as it takes the
    /// uploader over the per-user quota. Fails at once if nothing is left
    /// of the quota.
    ///
    /// The quota is checked against what is stored when the upload begins:
    /// uploads of one user that run at the same time can together exceed
    /// it by at most the largest attachment each.
    pub async fn begin_upload_for(
        &self,
        creator: &ParticipantId,
    ) -> Result<Upload, AttachmentError> {
        let quota = self.quota_left(creator).await?;
        if let Some((quota, 0)) = quota {
            return Err(AttachmentError::QuotaExceeded { quota });
        }
        let mut upload = self.begin_upload().await?;
        upload.quota = quota;
        Ok(upload)
    }

    /// Starts an upload that is not counted against anyone's quota.
    pub async fn begin_upload(&self) -> Result<Upload, AttachmentError> {
        let path = self.temporary_path()?;
        let file = tokio::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .await?;
        Ok(Upload {
            file: Some(file),
            path,
            size: 0,
            max_size: self.inner.settings.max_size,
            quota: None,
            hasher: Sha256::new(),
        })
    }

    /// Makes an upload the attachment `id` of a wavelet
    /// (`AttachmentService.storeAttachment`): syncs the file, moves it into
    /// place, makes the thumbnail if it is an image, and commits the
    /// metadata. Fails if the id is taken. The caller has checked that
    /// `creator` may write the wavelet.
    pub async fn finish_upload(
        &self,
        mut upload: Upload,
        id: &AttachmentId,
        wavelet_name: &WaveletName,
        file_name: &str,
        creator: Option<&ParticipantId>,
    ) -> Result<AttachmentMetadata, AttachmentError> {
        let _storing = self
            .claim_storing(id)
            .ok_or_else(|| AttachmentError::AlreadyExists(id.clone()))?;
        if self.metadata(id).await?.is_some() {
            return Err(AttachmentError::AlreadyExists(id.clone()));
        }
        let sha256: [u8; 32] = std::mem::take(&mut upload.hasher).finalize().into();
        let thumbnail = self.place(&mut upload, id).await?;
        let metadata = self.new_metadata(
            id,
            wavelet_name,
            file_name,
            upload.size,
            creator.map_or(String::new(), |creator| creator.address().to_owned()),
            thumbnail,
            sha256,
            self.inner.clock.now_millis(),
            None,
        );
        self.write_row(&metadata, None).await?;
        tracing::debug!(attachment = %id, size = metadata.size, "stored attachment");
        let listener = lock(&self.inner.listener).clone();
        if let Some(listener) = listener {
            listener.uploaded(&metadata);
        }
        Ok(metadata)
    }

    /// Marks an id as being stored; `None` if it is already.
    fn claim_storing(&self, id: &AttachmentId) -> Option<Storing<'_>> {
        let serialised = id.serialise();
        lock(&self.inner.storing)
            .insert(serialised.clone())
            .then(|| Storing {
                inner: &self.inner,
                id: serialised,
            })
    }

    /// Syncs a finished upload's file and moves it into place as the bytes
    /// of `id`, then makes its thumbnail if it is an image. The one way
    /// files get into the store, for uploads and for copies of other
    /// servers' attachments alike.
    async fn place(
        &self,
        upload: &mut Upload,
        id: &AttachmentId,
    ) -> Result<Option<Thumbnail>, AttachmentError> {
        let Some(mut file) = upload.file.take() else {
            return Err(AttachmentError::Io(std::io::Error::other(
                "upload already finished",
            )));
        };
        let temporary = upload.path.clone();
        let path = self.file_path(id);
        let moved = async {
            file.flush().await?;
            file.sync_all().await?;
            drop(file);
            let (from, to) = (temporary.clone(), path.clone());
            blocking(move || {
                let directory = to.parent().unwrap_or(Path::new("."));
                std::fs::create_dir_all(directory)?;
                // A file already there has no metadata: an orphan.
                std::fs::rename(&from, &to)?;
                sync_directory(directory)
            })
            .await
        };
        if let Err(error) = moved.await {
            let _ = tokio::fs::remove_file(&temporary).await;
            return Err(error.into());
        }
        Ok(self.make_thumbnail(&path, &self.thumbnail_path(id)).await)
    }

    #[allow(clippy::too_many_arguments)]
    fn new_metadata(
        &self,
        id: &AttachmentId,
        wavelet_name: &WaveletName,
        file_name: &str,
        size: u64,
        creator: String,
        thumbnail: Option<Thumbnail>,
        sha256: [u8; 32],
        created_at: i64,
        origin: Option<String>,
    ) -> AttachmentMetadata {
        let file_name = sanitize_file_name(file_name);
        AttachmentMetadata {
            id: id.clone(),
            wave_ref: wavelet_name_to_wave_ref(wavelet_name),
            mime_type: mime::mime_type_for(&file_name).to_owned(),
            file_name,
            size,
            creator,
            image: thumbnail.as_ref().map(|thumbnail| ImageSize {
                width: thumbnail.image_width,
                height: thumbnail.image_height,
            }),
            thumbnail: Some(match &thumbnail {
                Some(thumbnail) => ImageSize {
                    width: thumbnail.width,
                    height: thumbnail.height,
                },
                None => ImageSize {
                    width: PATTERN_WIDTH,
                    height: PATTERN_HEIGHT,
                },
            }),
            // No scanner is configured; uploads and fetched copies alike.
            malware: None,
            sha256,
            created_at,
            origin,
            evicted: false,
        }
    }

    /// Commits the metadata of files just placed; removes the files if
    /// that fails. A copy of another server's attachment (`last_used` set)
    /// replaces its evicted row; an upload never replaces anything.
    async fn write_row(
        &self,
        metadata: &AttachmentMetadata,
        last_used: Option<i64>,
    ) -> Result<(), AttachmentError> {
        let row = metadata.clone();
        let written = self
            .inner
            .db
            .write(move |conn| {
                let size = |size: Option<ImageSize>| match size {
                    Some(size) => (Some(size.width), Some(size.height)),
                    None => (None, None),
                };
                let (image_width, image_height) = size(row.image);
                let (thumbnail_width, thumbnail_height) = size(row.thumbnail);
                let upsert = if last_used.is_some() {
                    " ON CONFLICT (id) DO UPDATE SET image_width = excluded.image_width, \
                     image_height = excluded.image_height, \
                     thumbnail_width = excluded.thumbnail_width, \
                     thumbnail_height = excluded.thumbnail_height, \
                     last_used = excluded.last_used, evicted = 0 \
                     WHERE attachments.sha256 = excluded.sha256"
                } else {
                    ""
                };
                conn.prepare_cached(&format!(
                    "INSERT INTO attachments (id, wave_ref, file_name, mime_type, size, creator, \
                     image_width, image_height, thumbnail_width, thumbnail_height, malware, \
                     sha256, created_at, origin, last_used) \
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)\
                     {upsert}"
                ))?
                .execute(params![
                    row.id.serialise(),
                    row.wave_ref,
                    row.file_name,
                    row.mime_type,
                    i64::try_from(row.size).unwrap_or(i64::MAX),
                    row.creator,
                    image_width,
                    image_height,
                    thumbnail_width,
                    thumbnail_height,
                    row.malware,
                    row.sha256.as_slice(),
                    row.created_at,
                    row.origin,
                    last_used
                ])?;
                Ok(())
            })
            .await;
        if let Err(error) = written {
            let _ = tokio::fs::remove_file(self.file_path(&metadata.id)).await;
            let _ = tokio::fs::remove_file(self.thumbnail_path(&metadata.id)).await;
            return Err(error.into());
        }
        Ok(())
    }

    // ---- Attachments of other servers ----

    /// Installs what fetches other servers' attachments (federation).
    /// Returns false if one was installed already.
    pub fn set_origin(&self, origin: Arc<dyn AttachmentOrigin>) -> bool {
        self.inner.origin.set(origin).is_ok()
    }

    /// Whether attachments of other servers can be fetched.
    pub fn fetches_remote(&self) -> bool {
        self.inner.origin.get().is_some()
    }

    /// Whether the attachment was uploaded to another server: its id names
    /// a domain other than this server's.
    pub fn is_remote(&self, id: &AttachmentId) -> bool {
        !id.domain().is_empty() && id.domain() != self.inner.settings.domain
    }

    /// Whether an attachment missing here may be fetched from elsewhere:
    /// it is another server's, (person mode) another device's of this
    /// domain, or its id names no domain, in which case it may be on the
    /// server of anyone on its wavelet.
    pub fn is_fetchable(&self, id: &AttachmentId) -> bool {
        self.fetches_remote()
            && (id.domain().is_empty()
                || self.is_remote(id)
                || (self.inner.settings.shared_domain && id.domain() == self.inner.settings.domain))
    }

    /// The bytes of another server's attachment of `wavelet`, fetched from
    /// that server into the cache here; the caller has checked that its
    /// user may read the wavelet. Everyone asking for the same attachment
    /// while it is being fetched shares one fetch. An origin found
    /// unreachable is not asked again for a few seconds.
    pub async fn fetch_remote(
        &self,
        id: &AttachmentId,
        wavelet: &WaveletName,
    ) -> Result<AttachmentMetadata, FetchError> {
        let Some(origin) = self.inner.origin.get().cloned() else {
            return Err(FetchError::NotFound);
        };
        if !self.is_fetchable(id) {
            return Err(FetchError::NotFound);
        }
        let domain = id.origin_domain(wavelet).to_owned();
        if let Some((until, message)) = lock(&self.inner.unavailable).get(&domain).cloned()
            && Instant::now() < until
        {
            return Err(FetchError::Unavailable { domain, message });
        }
        let result = self
            .fetch_shared(id, wavelet, origin.as_ref(), self.inner.settings.max_size)
            .await;
        let mut unavailable = lock(&self.inner.unavailable);
        match &result {
            Err(FetchError::Unavailable { message, .. }) => {
                unavailable.insert(
                    domain,
                    (Instant::now() + UNAVAILABLE_BACKOFF, message.clone()),
                );
            }
            Ok(_) => {
                unavailable.remove(&domain);
            }
            Err(_) => {}
        }
        result
    }

    /// One fetch of `id` through `origin` into the cache, shared with
    /// everyone asking for it meanwhile; at most `limit` bytes.
    async fn fetch_shared(
        &self,
        id: &AttachmentId,
        wavelet: &WaveletName,
        origin: &dyn AttachmentOrigin,
        limit: u64,
    ) -> Result<AttachmentMetadata, FetchError> {
        let key = (id.serialise(), wavelet.to_string());
        let fetch = Arc::clone(lock(&self.inner.fetches).entry(key.clone()).or_default());
        let result = fetch
            .get_or_init(|| async {
                self.inner
                    .remote_fetches
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                let mut upload = self
                    .begin_upload()
                    .await
                    .map_err(|e| FetchError::Local(e.to_string()))?;
                upload.max_size = upload.max_size.min(limit);
                let remote = origin.fetch(id, wavelet, &mut upload).await?;
                self.finish_fetch(upload, id, wavelet, remote).await
            })
            .await
            .clone();
        let mut fetches = lock(&self.inner.fetches);
        if fetches.get(&key).is_some_and(|f| Arc::ptr_eq(f, &fetch)) {
            fetches.remove(&key);
        }
        result
    }

    /// Person mode: copies an attachment of this domain held by another
    /// device of the person into the cache ahead of anyone asking for it,
    /// through `origin` (which asks that device, without waking anything)
    /// and like any fetched copy: size and SHA-256 checked, thumbnail
    /// made, counted against the cache budget. At most `limit` bytes. Does
    /// not consult or record the back-off for unreachable origins: a
    /// background copy failing says nothing to a user asking.
    pub async fn fetch_copy(
        &self,
        id: &AttachmentId,
        wavelet: &WaveletName,
        origin: &dyn AttachmentOrigin,
        limit: u64,
    ) -> Result<AttachmentMetadata, FetchError> {
        if !self.is_fetchable(id) {
            return Err(FetchError::NotFound);
        }
        self.fetch_shared(id, wavelet, origin, limit).await
    }

    /// Bytes of fetched copies held (not evicted).
    pub async fn cache_usage(&self) -> Result<u64, AttachmentError> {
        Ok(self
            .inner
            .db
            .read(|conn| {
                let total: i64 = conn
                    .prepare_cached(
                        "SELECT COALESCE(SUM(size), 0) FROM attachments \
                         WHERE origin IS NOT NULL AND evicted = 0",
                    )?
                    .query_row([], |row| row.get(0))?;
                Ok(total.max(0) as u64)
            })
            .await?)
    }

    /// How many more bytes of fetched copies fit the cache budget before
    /// anything is evicted.
    pub async fn cache_free(&self) -> Result<u64, AttachmentError> {
        Ok(self
            .inner
            .settings
            .cache_budget
            .saturating_sub(self.cache_usage().await?))
    }

    /// Person mode: the attachments held here (uploaded here or copied,
    /// not evicted) whose ids name `domain`, ordered by creation time and
    /// id, after `after` (a creation time and id); at most `limit`.
    pub async fn held_of_domain(
        &self,
        domain: &str,
        after: Option<(i64, String)>,
        limit: usize,
    ) -> Result<Vec<AttachmentMetadata>, AttachmentError> {
        let prefix = format!("{domain}/");
        let (after_at, after_id) = after.unwrap_or((i64::MIN, String::new()));
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
        Ok(self
            .inner
            .db
            .read(move |conn| {
                let mut statement = conn.prepare_cached(&format!(
                    "SELECT {COLUMNS} FROM attachments \
                     WHERE substr(id, 1, length(?1)) = ?1 AND evicted = 0 \
                     AND (created_at > ?2 OR (created_at = ?2 AND id > ?3)) \
                     ORDER BY created_at, id LIMIT ?4"
                ))?;
                let rows =
                    statement.query_map(params![prefix, after_at, after_id, limit], read_row)?;
                let mut list = Vec::new();
                for row in rows {
                    list.extend(row?);
                }
                Ok(list)
            })
            .await?)
    }

    /// Installs what is told of uploads (federation), replacing any
    /// before; `None` removes it.
    pub fn set_upload_listener(&self, listener: Option<Arc<dyn UploadListener>>) {
        *lock(&self.inner.listener) = listener;
    }

    /// How many fetches from other servers were started; for tests and
    /// diagnostics.
    pub fn remote_fetch_count(&self) -> u64 {
        self.inner
            .remote_fetches
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Checks what an origin sent against what it announced and what was
    /// pinned for the id before, then stores it like an upload.
    async fn finish_fetch(
        &self,
        mut upload: Upload,
        id: &AttachmentId,
        wavelet: &WaveletName,
        remote: RemoteAttachment,
    ) -> Result<AttachmentMetadata, FetchError> {
        let local = |error: AttachmentError| FetchError::Local(error.to_string());
        let Some(_storing) = self.claim_storing(id) else {
            return Err(FetchError::Local(format!("{id} is being stored")));
        };
        let pinned = self.metadata(id).await.map_err(local)?;
        if let Some(pinned) = &pinned {
            if pinned.origin.is_none() || !pinned.evicted {
                return Ok(pinned.clone());
            }
            if pinned.sha256 != remote.sha256 || pinned.size != remote.size {
                return Err(FetchError::Rejected(format!(
                    "{id} is not what it was when first fetched"
                )));
            }
        }
        if upload.size != remote.size {
            return Err(FetchError::Rejected(format!(
                "{} bytes sent, {} announced",
                upload.size, remote.size
            )));
        }
        let sha256: [u8; 32] = std::mem::take(&mut upload.hasher).finalize().into();
        if sha256 != remote.sha256 {
            return Err(FetchError::Rejected(
                "the bytes do not have the announced SHA-256".to_owned(),
            ));
        }
        let thumbnail = self.place(&mut upload, id).await.map_err(local)?;
        let now = self.inner.clock.now_millis();
        let metadata = self.new_metadata(
            id,
            wavelet,
            &remote.file_name,
            remote.size,
            remote.creator,
            thumbnail,
            sha256,
            remote.created_at,
            Some(remote.origin.clone()),
        );
        self.write_row(&metadata, Some(now)).await.map_err(local)?;
        tracing::debug!(attachment = %id, size = metadata.size, "cached attachment of another server");
        if let Err(error) = self.evict_over_budget(id).await {
            tracing::warn!(%error, "cannot evict cached attachments");
        }
        Ok(metadata)
    }

    /// Notes that a cached copy of another server's attachment was used.
    pub async fn touch(&self, id: &AttachmentId) -> Result<(), AttachmentError> {
        let serialised = id.serialise();
        let now = self.inner.clock.now_millis();
        self.inner
            .db
            .write(move |conn| {
                conn.prepare_cached(
                    "UPDATE attachments SET last_used = ?2 WHERE id = ?1 AND origin IS NOT NULL",
                )?
                .execute(params![serialised, now])?;
                Ok(())
            })
            .await?;
        Ok(())
    }

    /// Drops the files of the least recently used cached copies until the
    /// cache fits its budget again, except those of `keep`. Their rows
    /// stay, marked evicted, so that their hashes stay pinned.
    async fn evict_over_budget(&self, keep: &AttachmentId) -> Result<(), AttachmentError> {
        let budget = i64::try_from(self.inner.settings.cache_budget).unwrap_or(i64::MAX);
        let keep = keep.serialise();
        let evicted: Vec<String> = self
            .inner
            .db
            .write(move |conn| {
                let total: i64 = conn
                    .prepare_cached(
                        "SELECT COALESCE(SUM(size), 0) FROM attachments \
                         WHERE origin IS NOT NULL AND evicted = 0",
                    )?
                    .query_row([], |row| row.get(0))?;
                let mut over = total.saturating_sub(budget);
                let mut evicted = Vec::new();
                if over <= 0 {
                    return Ok(evicted);
                }
                let mut statement = conn.prepare_cached(
                    "SELECT id, size FROM attachments WHERE origin IS NOT NULL AND evicted = 0 \
                     AND id != ?1 ORDER BY last_used, id",
                )?;
                let rows = statement.query_map([&keep], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
                })?;
                for row in rows {
                    let (id, size) = row?;
                    evicted.push(id);
                    over -= size;
                    if over <= 0 {
                        break;
                    }
                }
                let mut update =
                    conn.prepare_cached("UPDATE attachments SET evicted = 1 WHERE id = ?1")?;
                for id in &evicted {
                    update.execute([id])?;
                }
                Ok(evicted)
            })
            .await?;
        for id in evicted {
            let Ok(id) = AttachmentId::deserialise(&id) else {
                continue;
            };
            tracing::debug!(attachment = %id, "evicted cached attachment");
            for path in [self.file_path(&id), self.thumbnail_path(&id)] {
                match tokio::fs::remove_file(&path).await {
                    Ok(()) => {}
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    Err(error) => return Err(error.into()),
                }
            }
        }
        Ok(())
    }

    /// Makes and stores the thumbnail of the file at `path` if it is an
    /// image that can be decoded within the limits.
    async fn make_thumbnail(&self, path: &Path, thumbnail_path: &Path) -> Option<Thumbnail> {
        let _permit = self.inner.thumbnails.acquire().await.ok()?;
        let (path, thumbnail_path) = (path.to_path_buf(), thumbnail_path.to_path_buf());
        let limits = self.inner.settings.decode_limits;
        let temporary = self.temporary_path().ok()?;
        let made = tokio::task::spawn_blocking(move || {
            let thumbnail = thumbnail::make_thumbnail(&path, limits)?;
            let stored = (|| {
                std::fs::write(&temporary, &thumbnail.jpeg)?;
                std::fs::File::open(&temporary)?.sync_all()?;
                std::fs::rename(&temporary, &thumbnail_path)
            })();
            match stored {
                Ok(()) => Some(thumbnail),
                Err(error) => {
                    tracing::warn!(%error, "cannot store attachment thumbnail");
                    let _ = std::fs::remove_file(&temporary);
                    None
                }
            }
        })
        .await;
        made.ok().flatten()
    }

    /// Stores bytes as the attachment `id` of a wavelet.
    pub async fn store(
        &self,
        id: &AttachmentId,
        wavelet_name: &WaveletName,
        file_name: &str,
        creator: Option<&ParticipantId>,
        bytes: &[u8],
    ) -> Result<AttachmentMetadata, AttachmentError> {
        let mut upload = self.begin_upload().await?;
        upload.write(bytes).await?;
        self.finish_upload(upload, id, wavelet_name, file_name, creator)
            .await
    }

    /// Opens the bytes of an attachment for reading; `None` if there is no
    /// file (`AttachmentService.getAttachment`). Returns the file and its
    /// length.
    pub async fn open_file(
        &self,
        id: &AttachmentId,
    ) -> Result<Option<(tokio::fs::File, u64)>, AttachmentError> {
        open(&self.file_path(id)).await
    }

    /// Opens the stored thumbnail of an image attachment
    /// (`AttachmentService.getThumbnail`).
    pub async fn open_thumbnail(
        &self,
        id: &AttachmentId,
    ) -> Result<Option<(tokio::fs::File, u64)>, AttachmentError> {
        open(&self.thumbnail_path(id)).await
    }

    /// The bytes of an attachment, whole. For attachments within the
    /// configured size limit; use [`Self::open_file`] to stream.
    pub async fn read(&self, id: &AttachmentId) -> Result<Option<Vec<u8>>, AttachmentError> {
        if self.metadata(id).await?.is_none() {
            return Ok(None);
        }
        match tokio::fs::read(self.file_path(id)).await {
            Ok(bytes) => Ok(Some(bytes)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error.into()),
        }
    }

    /// Removes an attachment: its metadata first, then its files
    /// (`AttachmentStore.deleteAttachment`). Returns whether it existed.
    pub async fn delete(&self, id: &AttachmentId) -> Result<bool, AttachmentError> {
        let serialised = id.serialise();
        let existed = self
            .inner
            .db
            .write(move |conn| {
                Ok(conn
                    .prepare_cached("DELETE FROM attachments WHERE id = ?1")?
                    .execute([&serialised])?
                    > 0)
            })
            .await?;
        for path in [self.file_path(id), self.thumbnail_path(id)] {
            match tokio::fs::remove_file(&path).await {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
        }
        Ok(existed)
    }

    /// Removes what no attachment owns and is older than `min_age`:
    /// abandoned uploads, and files without a metadata row (left by a
    /// crash between storing a file and committing its row).
    pub async fn sweep(&self, min_age: Duration) -> Result<SweepReport, AttachmentError> {
        let known: HashSet<String> = self
            .inner
            .db
            .read(|conn| {
                let mut statement = conn.prepare("SELECT id FROM attachments WHERE evicted = 0")?;
                let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
                let mut stems = HashSet::new();
                for row in rows {
                    if let Ok(id) = AttachmentId::deserialise(&row?) {
                        stems.insert(id.file_stem());
                    }
                }
                Ok(stems)
            })
            .await?;
        // Ids being stored have files but no row yet.
        let storing: HashSet<String> = lock(&self.inner.storing)
            .iter()
            .filter_map(|id| AttachmentId::deserialise(id).ok())
            .map(|id| id.file_stem())
            .collect();
        let directory = self.inner.settings.directory.clone();
        let report = blocking(move || {
            let old = |path: &Path| {
                std::fs::metadata(path)
                    .and_then(|metadata| metadata.modified())
                    .ok()
                    .and_then(|modified| SystemTime::now().duration_since(modified).ok())
                    .is_some_and(|age| age >= min_age)
            };
            let mut report = SweepReport::default();
            for entry in std::fs::read_dir(directory.join("tmp"))?.flatten() {
                let path = entry.path();
                if old(&path) && std::fs::remove_file(&path).is_ok() {
                    report.temporary_files += 1;
                }
            }
            for shard in std::fs::read_dir(directory.join("files"))?.flatten() {
                let Ok(files) = std::fs::read_dir(shard.path()) else {
                    continue;
                };
                for entry in files.flatten() {
                    let name = entry.file_name().to_string_lossy().into_owned();
                    let stem = name.strip_suffix(".thumb").unwrap_or(&name);
                    if known.contains(stem) || storing.contains(stem) {
                        continue;
                    }
                    let path = entry.path();
                    if old(&path) && std::fs::remove_file(&path).is_ok() {
                        report.orphans += 1;
                    }
                }
            }
            Ok(report)
        })
        .await?;
        if report != SweepReport::default() {
            tracing::info!(
                temporary_files = report.temporary_files,
                orphans = report.orphans,
                "removed unowned attachment files"
            );
        }
        Ok(report)
    }

    /// Starts a task that sweeps once now and then every `interval`.
    pub fn spawn_maintenance(&self, interval: Duration) -> tokio::task::JoinHandle<()> {
        let inner = Arc::downgrade(&self.inner);
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(interval);
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                ticker.tick().await;
                let Some(inner) = inner.upgrade() else { break };
                let service = AttachmentService { inner };
                if let Err(error) = service.sweep(ORPHAN_MIN_AGE).await {
                    tracing::warn!(%error, "sweeping the attachments directory failed");
                }
            }
        })
    }
}

/// How old a file without a metadata row must be before the periodic sweep
/// removes it. Longer than any upload takes.
pub const ORPHAN_MIN_AGE: Duration = Duration::from_secs(60 * 60);

async fn open(path: &Path) -> Result<Option<(tokio::fs::File, u64)>, AttachmentError> {
    match tokio::fs::File::open(path).await {
        Ok(file) => {
            let length = file.metadata().await?.len();
            Ok(Some((file, length)))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

/// The attachment operations other parts of the server need (robots
/// importing and exporting waves, for one), without the HTTP side.
/// Implemented by [`AttachmentService`]. Access control is the caller's
/// business.
pub trait AttachmentStore: Send + Sync {
    /// The metadata of an attachment, if it exists.
    fn metadata(
        &self,
        id: &AttachmentId,
    ) -> impl Future<Output = Result<Option<AttachmentMetadata>, AttachmentError>> + Send;

    /// The bytes of an attachment, if it exists.
    fn read(
        &self,
        id: &AttachmentId,
    ) -> impl Future<Output = Result<Option<Vec<u8>>, AttachmentError>> + Send;

    /// Stores bytes as a new attachment of a wavelet; fails if the id is
    /// taken or the bytes exceed the size limit.
    fn store(
        &self,
        id: &AttachmentId,
        wavelet_name: &WaveletName,
        file_name: &str,
        creator: Option<&ParticipantId>,
        bytes: &[u8],
    ) -> impl Future<Output = Result<AttachmentMetadata, AttachmentError>> + Send;

    /// Removes an attachment; whether it existed.
    fn delete(
        &self,
        id: &AttachmentId,
    ) -> impl Future<Output = Result<bool, AttachmentError>> + Send;
}

impl AttachmentStore for AttachmentService {
    async fn metadata(
        &self,
        id: &AttachmentId,
    ) -> Result<Option<AttachmentMetadata>, AttachmentError> {
        AttachmentService::metadata(self, id).await
    }

    async fn read(&self, id: &AttachmentId) -> Result<Option<Vec<u8>>, AttachmentError> {
        AttachmentService::read(self, id).await
    }

    async fn store(
        &self,
        id: &AttachmentId,
        wavelet_name: &WaveletName,
        file_name: &str,
        creator: Option<&ParticipantId>,
        bytes: &[u8],
    ) -> Result<AttachmentMetadata, AttachmentError> {
        AttachmentService::store(self, id, wavelet_name, file_name, creator, bytes).await
    }

    async fn delete(&self, id: &AttachmentId) -> Result<bool, AttachmentError> {
        AttachmentService::delete(self, id).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_parsed_as_in_the_original() {
        let id = AttachmentId::deserialise("example.com/att+1").unwrap();
        assert_eq!((id.domain(), id.id()), ("example.com", "att+1"));
        assert_eq!(id.serialise(), "example.com/att+1");
        let legacy = AttachmentId::deserialise("att+1").unwrap();
        assert_eq!((legacy.domain(), legacy.id()), ("", "att+1"));
        assert_eq!(legacy.serialise(), "att+1");
        assert_ne!(id, legacy);
        // An id that names no domain is of the server hosting the wavelet.
        let wavelet = wave_ref_to_wavelet_name("b.example/w+abc").unwrap();
        assert_eq!(id.origin_domain(&wavelet), "example.com");
        assert_eq!(legacy.origin_domain(&wavelet), "b.example");
        for bad in [
            "",
            "/",
            "a/b/c",
            "../../etc/passwd",
            "example.com/",
            "/x",
            "a\nb",
            "a\0b",
            &"x".repeat(MAX_ID_LENGTH + 1),
        ] {
            assert!(AttachmentId::deserialise(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn file_names_never_come_from_the_id() {
        for hostile in [
            "..",
            ".",
            "example.com/..",
            "..\\..\\x",
            "a b",
            "%2e%2e",
            "~",
        ] {
            let id = AttachmentId::deserialise(hostile).unwrap();
            let stem = id.file_stem();
            assert_eq!(stem.len(), 64, "{hostile}");
            assert!(stem.bytes().all(|b| b.is_ascii_hexdigit()), "{hostile}");
        }
        let a = AttachmentId::deserialise("a/b").unwrap().file_stem();
        let b = AttachmentId::deserialise("a").unwrap().file_stem();
        assert_ne!(a, b);
    }

    #[test]
    fn uploaded_file_names_are_reduced_to_a_base_name() {
        assert_eq!(sanitize_file_name("photo.png"), "photo.png");
        assert_eq!(sanitize_file_name("C:\\Users\\me\\photo.png"), "photo.png");
        assert_eq!(sanitize_file_name("/etc/../passwd"), "passwd");
        assert_eq!(sanitize_file_name("a\r\nb\0.txt"), "ab.txt");
        assert_eq!(sanitize_file_name("dir/"), "");
        assert_eq!(sanitize_file_name(&"x".repeat(1000)).len(), 255);
        assert_eq!(
            sanitize_file_name("caf\u{e9} \u{1F600}.txt"),
            "caf\u{e9} \u{1F600}.txt"
        );
    }

    #[test]
    fn wave_references_name_wavelets() {
        let name =
            WaveletName::of_parts("example.com", "w+abc", "example.com", "conv+root").unwrap();
        let wave_ref = wavelet_name_to_wave_ref(&name);
        assert_eq!(wave_ref, "example.com/w+abc/~/conv+root");
        assert_eq!(wave_ref_to_wavelet_name(&wave_ref), Some(name.clone()));
        // A reference to a wave means its root conversation wavelet.
        assert_eq!(wave_ref_to_wavelet_name("example.com/w+abc"), Some(name));
        assert_eq!(wave_ref_to_wavelet_name("nonsense"), None);
    }
}
