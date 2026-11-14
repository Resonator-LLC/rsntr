//! Names and avatars people set for themselves.
//!
//! The profile of an account of this domain is kept here (`own`); the
//! profile of an address of another domain is fetched from that domain
//! through federation ([`RemoteProfiles`], installed by
//! `resonator-federation`) and kept as a copy, refreshed when it is older
//! than [`REFRESH_AFTER_MS`]. Without a stored name the name is derived
//! from the address, and without an avatar the initials are drawn.
//!
//! Names are not unique: the client shows the address next to them, so a
//! name alone never stands for someone.

use std::collections::HashMap;
use std::io::Cursor;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use futures_util::future::BoxFuture;
use image::{ImageFormat, ImageReader, Limits};
use resonator_model::id::ParticipantId;

use crate::attachment::thumbnail::DecodeLimits;
use crate::clock::Clock;
use crate::storage::{Database, ProfileRecord, ProfileStore, StorageError, StoredProfile};

/// Most characters in a name.
pub const MAX_NAME_CHARS: usize = 64;
/// Width and height of stored avatars, in pixels.
pub const AVATAR_PX: u32 = 256;
/// Largest avatar upload, in bytes.
pub const MAX_AVATAR_UPLOAD: usize = 5 * 1024 * 1024;
/// Largest stored (or fetched) avatar PNG, in bytes.
pub const MAX_AVATAR_PNG: usize = 512 * 1024;
/// A copy older than this is fetched again when someone looks at it.
pub const REFRESH_AFTER_MS: i64 = 60 * 60 * 1000;
/// After a failed fetch, the domain is not asked again for this long.
const RETRY_AFTER_MS: i64 = 10 * 60 * 1000;
/// How long a profile request waits for profiles never fetched before.
const FIRST_FETCH_WAIT: Duration = Duration::from_secs(3);

/// Fetches profiles from the domains of their addresses (federation).
pub trait RemoteProfiles: Send + Sync {
    /// Asks the domains of `addresses` (all of other domains) for newer
    /// versions than `have` (the `updated_at` stored, by address).
    /// Avatars are checked by the caller.
    fn fetch<'a>(
        &'a self,
        addresses: &'a [ParticipantId],
        have: &'a HashMap<String, i64>,
    ) -> BoxFuture<'a, Fetched>;
}

/// What the domains answered.
#[derive(Debug, Default)]
pub struct Fetched {
    /// Addresses whose domain answered (with or without a newer version).
    pub answered: Vec<String>,
    /// The newer versions.
    pub records: Vec<ProfileRecord>,
}

/// Why a name is refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NameError {
    TooLong,
    Invisible,
}

impl std::fmt::Display for NameError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            NameError::TooLong => "A name has at most 64 characters.",
            NameError::Invisible => "A name cannot hold control or text-direction characters.",
        })
    }
}

/// Characters that are never in a name: controls, and those that change
/// the direction or hide text (so a name cannot disguise itself).
fn forbidden(c: char) -> bool {
    c.is_control()
        || matches!(c,
            '\u{200B}'..='\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2060}'..='\u{2069}' | '\u{FEFF}')
}

/// The name to keep: trimmed, inner runs of white space as one space.
/// Empty means "derive it from the address".
pub fn clean_name(input: &str) -> Result<String, NameError> {
    if input.chars().any(|c| forbidden(c) && !c.is_whitespace()) {
        return Err(NameError::Invisible);
    }
    let name = input.split_whitespace().collect::<Vec<_>>().join(" ");
    if name.chars().count() > MAX_NAME_CHARS {
        return Err(NameError::TooLong);
    }
    Ok(name)
}

/// The avatar made from an uploaded (or fetched) image: the centre square,
/// at most [`AVATAR_PX`] wide, as PNG without metadata. `None` if it is
/// not a PNG, JPEG, GIF or WebP image within `limits`.
/// Decoding is CPU-bound: call it off the async runtime.
pub fn make_avatar(bytes: &[u8], limits: DecodeLimits) -> Option<Vec<u8>> {
    let reader = ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .ok()?;
    let format = reader.format()?;
    if !matches!(
        format,
        ImageFormat::Png | ImageFormat::Jpeg | ImageFormat::Gif | ImageFormat::WebP
    ) {
        return None;
    }
    let (width, height) = reader.into_dimensions().ok()?;
    if width == 0
        || height == 0
        || width > limits.max_dimension
        || height > limits.max_dimension
        || u64::from(width) * u64::from(height) > limits.max_pixels
    {
        return None;
    }
    let mut reader = ImageReader::new(Cursor::new(bytes));
    reader.set_format(format);
    let mut decoder_limits = Limits::default();
    decoder_limits.max_image_width = Some(limits.max_dimension);
    decoder_limits.max_image_height = Some(limits.max_dimension);
    decoder_limits.max_alloc = Some(limits.max_alloc);
    reader.limits(decoder_limits);
    let image = reader.decode().ok()?;
    let side = width.min(height);
    let square = image.crop_imm((width - side) / 2, (height - side) / 2, side, side);
    let size = side.min(AVATAR_PX);
    let scaled = square.thumbnail_exact(size, size).into_rgba8();
    let mut png = Vec::new();
    scaled
        .write_to(&mut Cursor::new(&mut png), ImageFormat::Png)
        .ok()?;
    (png.len() <= MAX_AVATAR_PNG).then_some(png)
}

/// What a client is shown for an address.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShownProfile {
    pub address: String,
    pub name: String,
    /// Same-origin path of the avatar.
    pub image_url: String,
    /// Whether a name was set (not derived from the address).
    pub custom_name: bool,
    pub has_avatar: bool,
}

/// The display name derived from an address: the capitalised parts of its
/// name (`FetchProfilesService.SIMPLE_PROFILES_FETCHER`).
pub fn derived_name(address: &str) -> String {
    let name = address.split('@').next().unwrap_or("");
    if name.is_empty() {
        // The shared domain participant, `@example.com`.
        return address.to_owned();
    }
    name.split(['.', '_'])
        .filter(|part| !part.is_empty())
        .map(|part| {
            let mut chars = part.chars();
            match chars.next() {
                Some(first) => first.to_uppercase().chain(chars).collect::<String>(),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Profiles for the HTTP handlers; cheap to clone.
#[derive(Clone)]
pub struct Profiles(Arc<Inner>);

struct Inner {
    store: ProfileStore,
    domain: String,
    clock: Arc<dyn Clock>,
    limits: DecodeLimits,
    remote: OnceLock<Arc<dyn RemoteProfiles>>,
    /// Addresses whose domain did not answer, and when it was asked.
    failed: Mutex<HashMap<String, i64>>,
}

impl Profiles {
    pub fn new(db: &Database, domain: &str, clock: Arc<dyn Clock>, limits: DecodeLimits) -> Self {
        Profiles(Arc::new(Inner {
            store: db.profile_store(),
            domain: domain.to_owned(),
            clock,
            limits,
            remote: OnceLock::new(),
            failed: Mutex::new(HashMap::new()),
        }))
    }

    /// Installs the federation side. Returns false if one was installed.
    pub fn set_remote(&self, remote: Arc<dyn RemoteProfiles>) -> bool {
        self.0.remote.set(remote).is_ok()
    }

    pub fn store(&self) -> &ProfileStore {
        &self.0.store
    }

    /// Whether `address` is an account of this domain.
    pub fn is_own(&self, address: &str) -> bool {
        ParticipantId::of(address).is_ok_and(|id| id.domain() == self.0.domain)
    }

    fn shown(address: &str, stored: Option<&StoredProfile>) -> ShownProfile {
        let custom = stored.filter(|p| !p.name.is_empty());
        let avatar = stored.and_then(|p| p.avatar_sha256.as_deref());
        ShownProfile {
            address: address.to_owned(),
            name: custom.map_or_else(|| derived_name(address), |p| p.name.clone()),
            image_url: match avatar {
                // The hash in the URL: a new avatar is a new URL.
                Some(hash) => format!("/avatar/{address}?v={}", hex(&hash[..hash.len().min(6)])),
                // The initials follow the name: a new name is a new URL.
                None => match custom {
                    Some(p) => format!(
                        "/iniavatars/100x100/{address}?n={}",
                        hex(&<sha2::Sha256 as sha2::Digest>::digest(p.name.as_bytes())[..4])
                    ),
                    None => format!("/iniavatars/100x100/{address}"),
                },
            },
            custom_name: custom.is_some(),
            has_avatar: avatar.is_some(),
        }
    }

    /// What to show for these addresses. Copies of other domains' profiles
    /// that are missing are fetched first (waiting a little); stale ones
    /// are refreshed in the background.
    pub async fn show(&self, addresses: &[String]) -> Result<Vec<ShownProfile>, StorageError> {
        let mut stored = self.0.store.get_many(addresses).await?;
        if let Some(remote) = self.0.remote.get().cloned() {
            let now = self.0.clock.now_millis();
            let (mut missing, mut stale) = (Vec::new(), Vec::new());
            {
                let failed = self.0.failed.lock().unwrap_or_else(|e| e.into_inner());
                for address in addresses {
                    if self.is_own(address) || address.starts_with('@') {
                        continue;
                    }
                    let Ok(id) = ParticipantId::of(address) else {
                        continue;
                    };
                    if failed
                        .get(address)
                        .is_some_and(|at| now - at < RETRY_AFTER_MS)
                    {
                        continue;
                    }
                    match stored.get(address) {
                        None => missing.push(id),
                        Some(p) if now - p.fetched_at > REFRESH_AFTER_MS => stale.push(id),
                        Some(_) => {}
                    }
                }
            }
            if !stale.is_empty() {
                let this = self.clone();
                let remote = Arc::clone(&remote);
                tokio::spawn(async move { this.fetch(&*remote, &stale).await });
            }
            if !missing.is_empty() {
                let _ =
                    tokio::time::timeout(FIRST_FETCH_WAIT, self.fetch(&*remote, &missing)).await;
                stored = self.0.store.get_many(addresses).await?;
            }
        }
        Ok(addresses
            .iter()
            .map(|address| Self::shown(address, stored.get(address)))
            .collect())
    }

    /// Asks the domains of `addresses` and keeps what they answer.
    async fn fetch(&self, remote: &dyn RemoteProfiles, addresses: &[ParticipantId]) {
        let names = addresses
            .iter()
            .map(|a| a.address().to_owned())
            .collect::<Vec<_>>();
        let have = match self.0.store.get_many(&names).await {
            Ok(stored) => stored.into_iter().map(|(a, p)| (a, p.updated_at)).collect(),
            Err(_) => HashMap::new(),
        };
        let fetched = remote.fetch(addresses, &have).await;
        let now = self.0.clock.now_millis();
        for record in fetched.records {
            if !names.contains(&record.address) {
                continue;
            }
            let Ok(name) = clean_name(&record.name) else {
                continue;
            };
            // Made again here: what is stored and served is always our own PNG.
            let avatar = match record.avatar {
                Some(bytes) if bytes.len() <= MAX_AVATAR_PNG => {
                    let limits = self.0.limits;
                    tokio::task::spawn_blocking(move || make_avatar(&bytes, limits))
                        .await
                        .ok()
                        .flatten()
                }
                _ => None,
            };
            let record = ProfileRecord {
                name,
                avatar,
                ..record
            };
            if let Err(error) = self.0.store.put(record, false, now).await {
                tracing::warn!(%error, "could not keep a fetched profile");
            }
        }
        for address in &fetched.answered {
            // Nothing set there, or nothing newer: noted, so it is not
            // asked for again until the copy is stale.
            let empty = ProfileRecord {
                address: address.clone(),
                name: String::new(),
                avatar: None,
                updated_at: 0,
            };
            if let Err(error) = self.0.store.put(empty, false, now).await {
                tracing::warn!(%error, "could not note a fetched profile");
            }
        }
        let mut failed = self.0.failed.lock().unwrap_or_else(|e| e.into_inner());
        for address in &names {
            if fetched.answered.contains(address) {
                failed.remove(address);
            } else {
                failed.insert(address.clone(), now);
            }
        }
    }

    /// Keeps a version received from another device of the person (or
    /// any newer version of an own profile); returns whether it was newer.
    pub async fn learn_own(&self, record: ProfileRecord) -> Result<bool, StorageError> {
        let Ok(name) = clean_name(&record.name) else {
            return Ok(false);
        };
        let avatar = match record.avatar {
            Some(bytes) if bytes.len() <= MAX_AVATAR_PNG => {
                let limits = self.0.limits;
                match tokio::task::spawn_blocking(move || make_avatar(&bytes, limits)).await {
                    Ok(Some(png)) => Some(png),
                    _ => return Ok(false),
                }
            }
            Some(_) => return Ok(false),
            None => None,
        };
        let now = self.0.clock.now_millis();
        self.0
            .store
            .put(
                ProfileRecord {
                    name,
                    avatar,
                    ..record
                },
                true,
                now,
            )
            .await
    }

    /// Sets the name and/or avatar of an own account. `avatar`: `None`
    /// keeps it, `Some(None)` removes it, `Some(Some(png))` replaces it.
    pub async fn set_own(
        &self,
        address: &ParticipantId,
        name: Option<String>,
        avatar: Option<Option<Vec<u8>>>,
    ) -> Result<ProfileRecord, StorageError> {
        let current = self.0.store.record(address.address()).await?;
        // Later than the stored version even if the clock went back.
        let now = self.0.clock.now_millis();
        let updated_at = current.as_ref().map_or(now, |c| now.max(c.updated_at + 1));
        let record = ProfileRecord {
            address: address.address().to_owned(),
            name: name
                .unwrap_or_else(|| current.as_ref().map(|c| c.name.clone()).unwrap_or_default()),
            avatar: match avatar {
                Some(avatar) => avatar,
                None => current.and_then(|c| c.avatar),
            },
            updated_at,
        };
        self.0.store.put(record.clone(), true, now).await?;
        Ok(record)
    }

    pub fn limits(&self) -> DecodeLimits {
        self.0.limits
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_trimmed_and_checked() {
        assert_eq!(clean_name("  Anna   Maria \n").unwrap(), "Anna Maria");
        assert_eq!(clean_name("").unwrap(), "");
        assert_eq!(clean_name("\u{202E}evil"), Err(NameError::Invisible));
        assert_eq!(clean_name("a\u{200B}b"), Err(NameError::Invisible));
        assert_eq!(clean_name(&"x".repeat(65)), Err(NameError::TooLong));
        assert_eq!(
            clean_name(&"\u{e9}".repeat(64)).unwrap().chars().count(),
            64
        );
    }

    #[test]
    fn derived_names_follow_the_original() {
        assert_eq!(derived_name("john.smith@example.com"), "John Smith");
        assert_eq!(derived_name("john_smith@example.com"), "John Smith");
        assert_eq!(derived_name("john..smith.@example.com"), "John Smith");
        assert_eq!(derived_name("@example.com"), "@example.com");
    }

    #[test]
    fn avatars_are_square_pngs_of_at_most_256_px() {
        let mut source = Vec::new();
        image::RgbImage::from_pixel(800, 400, image::Rgb([200, 30, 30]))
            .write_to(&mut Cursor::new(&mut source), ImageFormat::Jpeg)
            .unwrap();
        let png = make_avatar(&source, DecodeLimits::default()).unwrap();
        let made = image::load_from_memory(&png).unwrap();
        assert_eq!((made.width(), made.height()), (256, 256));
        assert_eq!(image::guess_format(&png).unwrap(), ImageFormat::Png);
        // Small images are not enlarged.
        let mut small = Vec::new();
        image::RgbImage::new(40, 60)
            .write_to(&mut Cursor::new(&mut small), ImageFormat::Png)
            .unwrap();
        let made = image::load_from_memory(&make_avatar(&small, DecodeLimits::default()).unwrap())
            .unwrap();
        assert_eq!((made.width(), made.height()), (40, 40));
        assert_eq!(make_avatar(b"not an image", DecodeLimits::default()), None);
        let tiny = DecodeLimits {
            max_dimension: 100,
            ..DecodeLimits::default()
        };
        assert_eq!(make_avatar(&source, tiny), None);
    }
}
