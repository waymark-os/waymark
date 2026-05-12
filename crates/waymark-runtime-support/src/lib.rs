use std::env;
use std::ffi::OsString;
use std::path::{Path, PathBuf};

const WAYMARK_MOUNTS_ENV: &str = "WAYMARK_MOUNTS";

pub fn default_start_dir() -> PathBuf {
    env_start_dir()
        .or_else(preferred_start_dir)
        .or_else(|| env::current_dir().ok())
        .unwrap_or_else(|| PathBuf::from("/"))
}

pub fn boot_mount_manifest_from_env() -> Result<MountManifest, MountManifestError> {
    match env::var(WAYMARK_MOUNTS_ENV) {
        Ok(raw) => MountManifest::parse(&raw),
        Err(env::VarError::NotPresent) => Ok(MountManifest::default()),
        Err(env::VarError::NotUnicode(_)) => Err(MountManifestError::NonUtf8Env),
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct MountManifest {
    entries: Vec<MountSpec>,
}

impl MountManifest {
    pub fn parse(raw: &str) -> Result<Self, MountManifestError> {
        let raw = raw.trim();
        if raw.is_empty() {
            return Ok(Self::default());
        }

        let mut entries = Vec::new();
        for entry in raw.split(',') {
            entries.push(MountSpec::parse(entry.trim())?);
        }
        Ok(Self { entries })
    }

    pub fn entries(&self) -> &[MountSpec] {
        &self.entries
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MountSpec {
    pub mount_path: String,
    pub kind: MountKind,
    pub selector: MountSelector,
    pub writable: bool,
}

impl MountSpec {
    fn parse(raw: &str) -> Result<Self, MountManifestError> {
        let (mount_path, rest) = raw
            .split_once('=')
            .ok_or_else(|| MountManifestError::InvalidEntry(raw.to_owned()))?;
        validate_mount_path(mount_path)?;

        let mut fields = rest.split(':');
        let kind = fields
            .next()
            .filter(|field| !field.is_empty())
            .ok_or_else(|| MountManifestError::InvalidEntry(raw.to_owned()))?;
        let selector = fields
            .next()
            .filter(|field| !field.is_empty())
            .ok_or_else(|| MountManifestError::InvalidEntry(raw.to_owned()))?;
        let access = fields.next().unwrap_or("ro");
        if fields.next().is_some() {
            return Err(MountManifestError::InvalidEntry(raw.to_owned()));
        }

        Ok(Self {
            mount_path: mount_path.to_owned(),
            kind: MountKind::parse(kind),
            selector: MountSelector::parse(selector)?,
            writable: match access {
                "ro" => false,
                "rw" => true,
                _ => return Err(MountManifestError::InvalidAccess(access.to_owned())),
            },
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MountKind {
    Asset,
    Repo,
    Virtiofs,
    Other(String),
}

impl MountKind {
    fn parse(raw: &str) -> Self {
        match raw {
            "asset" => Self::Asset,
            "repo" => Self::Repo,
            "virtiofs" => Self::Virtiofs,
            other => Self::Other(other.to_owned()),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MountSelector {
    Label(String),
    Tag(String),
    Uuid(String),
    Block(String),
}

impl MountSelector {
    fn parse(raw: &str) -> Result<Self, MountManifestError> {
        if let Some(label) = raw.strip_prefix("label=") {
            validate_token("label", label)?;
            return Ok(Self::Label(label.to_owned()));
        }
        if let Some(tag) = raw.strip_prefix("tag=") {
            validate_token("tag", tag)?;
            return Ok(Self::Tag(tag.to_owned()));
        }
        if let Some(uuid) = raw.strip_prefix("uuid=") {
            validate_uuid(uuid)?;
            return Ok(Self::Uuid(uuid.to_ascii_lowercase()));
        }
        if let Some(block) = raw.strip_prefix("block=") {
            validate_token("block", block)?;
            return Ok(Self::Block(block.to_owned()));
        }
        if raw.starts_with("blk") {
            validate_token("block", raw)?;
            return Ok(Self::Block(raw.to_owned()));
        }
        Err(MountManifestError::InvalidSelector(raw.to_owned()))
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MountCandidate {
    pub block_name: String,
    pub kind: MountKind,
    pub uuid: Option<String>,
    pub label: Option<String>,
    pub writable: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolvedMount {
    pub mount_path: String,
    pub block_name: String,
    pub kind: MountKind,
    pub writable: bool,
}

pub fn resolve_mount_plan(
    manifest: &MountManifest,
    candidates: &[MountCandidate],
) -> Result<Vec<ResolvedMount>, MountResolveError> {
    let mut mounts = Vec::with_capacity(manifest.entries().len());
    for spec in manifest.entries() {
        let matches = candidates
            .iter()
            .filter(|candidate| mount_candidate_matches(spec, candidate))
            .collect::<Vec<_>>();
        let candidate = match matches.as_slice() {
            [] => return Err(MountResolveError::NotFound(spec.clone())),
            [candidate] => *candidate,
            _ => return Err(MountResolveError::Ambiguous(spec.clone())),
        };
        if spec.writable && !candidate.writable {
            return Err(MountResolveError::ReadOnlyCandidate {
                spec: spec.clone(),
                block_name: candidate.block_name.clone(),
            });
        }

        mounts.push(ResolvedMount {
            mount_path: spec.mount_path.clone(),
            block_name: candidate.block_name.clone(),
            kind: spec.kind.clone(),
            writable: spec.writable,
        });
    }
    Ok(mounts)
}

fn mount_candidate_matches(spec: &MountSpec, candidate: &MountCandidate) -> bool {
    if spec.kind != candidate.kind {
        return false;
    }
    match &spec.selector {
        MountSelector::Label(label) => candidate.label.as_deref() == Some(label.as_str()),
        MountSelector::Tag(tag) => candidate.label.as_deref() == Some(tag.as_str()),
        MountSelector::Uuid(uuid) => candidate
            .uuid
            .as_deref()
            .is_some_and(|candidate_uuid| uuid_eq(candidate_uuid, uuid)),
        MountSelector::Block(block_name) => candidate.block_name == *block_name,
    }
}

fn uuid_eq(left: &str, right: &str) -> bool {
    left.chars()
        .filter(|ch| *ch != '-')
        .map(|ch| ch.to_ascii_lowercase())
        .eq(right
            .chars()
            .filter(|ch| *ch != '-')
            .map(|ch| ch.to_ascii_lowercase()))
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MountResolveError {
    Ambiguous(MountSpec),
    NotFound(MountSpec),
    ReadOnlyCandidate { spec: MountSpec, block_name: String },
}

impl std::fmt::Display for MountResolveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Ambiguous(spec) => {
                write!(f, "mount {} matched multiple candidates", spec.mount_path)
            }
            Self::NotFound(spec) => write!(f, "mount {} matched no candidates", spec.mount_path),
            Self::ReadOnlyCandidate { spec, block_name } => write!(
                f,
                "mount {} requested rw but candidate {block_name} is read-only",
                spec.mount_path
            ),
        }
    }
}

impl std::error::Error for MountResolveError {}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MountManifestError {
    InvalidAccess(String),
    InvalidEntry(String),
    InvalidMountPath(String),
    InvalidSelector(String),
    InvalidToken { field: &'static str, value: String },
    InvalidUuid(String),
    NonUtf8Env,
}

impl std::fmt::Display for MountManifestError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidAccess(access) => write!(f, "invalid mount access {access}"),
            Self::InvalidEntry(entry) => write!(f, "invalid mount manifest entry {entry}"),
            Self::InvalidMountPath(path) => write!(f, "invalid mount path {path}"),
            Self::InvalidSelector(selector) => write!(f, "invalid mount selector {selector}"),
            Self::InvalidToken { field, value } => write!(f, "invalid {field} token {value}"),
            Self::InvalidUuid(uuid) => write!(f, "invalid mount UUID {uuid}"),
            Self::NonUtf8Env => write!(f, "{WAYMARK_MOUNTS_ENV} is not valid UTF-8"),
        }
    }
}

impl std::error::Error for MountManifestError {}

fn validate_mount_path(path: &str) -> Result<(), MountManifestError> {
    if path.starts_with('/')
        && path != "/"
        && !path.ends_with('/')
        && !path.contains("//")
        && path
            .split('/')
            .all(|part| part.is_empty() || part != "." && part != "..")
    {
        Ok(())
    } else {
        Err(MountManifestError::InvalidMountPath(path.to_owned()))
    }
}

fn validate_token(field: &'static str, value: &str) -> Result<(), MountManifestError> {
    if !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
    {
        Ok(())
    } else {
        Err(MountManifestError::InvalidToken {
            field,
            value: value.to_owned(),
        })
    }
}

fn validate_uuid(value: &str) -> Result<(), MountManifestError> {
    let hex = value.chars().filter(|ch| *ch != '-').collect::<String>();
    if hex.len() == 32 && hex.chars().all(|ch| ch.is_ascii_hexdigit()) {
        Ok(())
    } else {
        Err(MountManifestError::InvalidUuid(value.to_owned()))
    }
}

pub fn configure_process_environment(start_dir: &Path) {
    #[cfg(target_os = "hermit")]
    {
        let _ = env::set_current_dir(start_dir);

        // This runs at single-threaded process startup before guest workers exist.
        // SAFETY: Rust 2024 makes process environment mutation unsafe because concurrent
        // environment access can race. This function is called during guest startup before any
        // task server, worker, or user code can run, so no other thread can observe/mutate the
        // environment concurrently.
        unsafe {
            env::set_var("HOME", start_dir.as_os_str());
            env::set_var("TMPDIR", "/tmp");
            env::set_var("XDG_RUNTIME_DIR", "/run");
            if let Some(path) = build_search_path(start_dir, runtime_root()) {
                env::set_var("PATH", path);
            }
        }
    }

    #[cfg(not(target_os = "hermit"))]
    let _ = start_dir;
}

fn preferred_start_dir() -> Option<PathBuf> {
    ["/work", "/workspace"]
        .into_iter()
        .find(|path| Path::new(path).is_dir())
        .map(PathBuf::from)
}

fn env_start_dir() -> Option<PathBuf> {
    let path = env::var_os("WAYMARK_START_DIR")?;
    if path.is_empty() {
        return None;
    }
    Some(PathBuf::from(path))
}

#[cfg_attr(not(target_os = "hermit"), allow(dead_code))]
fn runtime_root() -> &'static Path {
    if Path::new("/runtime").is_dir() {
        Path::new("/runtime")
    } else {
        Path::new("/system")
    }
}

#[cfg_attr(not(target_os = "hermit"), allow(dead_code))]
fn build_search_path(start_dir: &Path, runtime_root: &Path) -> Option<OsString> {
    env::join_paths([start_dir.join(".venv/bin"), runtime_root.join("bin")]).ok()
}

#[cfg(test)]
mod tests {
    use super::{
        resolve_mount_plan, MountCandidate, MountKind, MountManifest, MountManifestError,
        MountResolveError, MountSelector,
    };

    #[test]
    fn parses_label_and_uuid_mounts() {
        let manifest = MountManifest::parse(
            "/task=asset:label=terminalbench-hello-world:ro,/repo=repo:uuid=00112233-4455-6677-8899-aabbccddeeff:rw",
        )
        .unwrap();

        assert_eq!(manifest.entries().len(), 2);
        assert_eq!(manifest.entries()[0].mount_path, "/task");
        assert_eq!(manifest.entries()[0].kind, MountKind::Asset);
        assert_eq!(
            manifest.entries()[0].selector,
            MountSelector::Label("terminalbench-hello-world".to_owned())
        );
        assert!(!manifest.entries()[0].writable);
        assert_eq!(manifest.entries()[1].mount_path, "/repo");
        assert_eq!(manifest.entries()[1].kind, MountKind::Repo);
        assert_eq!(
            manifest.entries()[1].selector,
            MountSelector::Uuid("00112233-4455-6677-8899-aabbccddeeff".to_owned())
        );
        assert!(manifest.entries()[1].writable);
    }

    #[test]
    fn parses_virtiofs_tag_mount() {
        let manifest = MountManifest::parse("/app=virtiofs:tag=app:rw").unwrap();

        assert_eq!(manifest.entries().len(), 1);
        assert_eq!(manifest.entries()[0].mount_path, "/app");
        assert_eq!(manifest.entries()[0].kind, MountKind::Virtiofs);
        assert_eq!(
            manifest.entries()[0].selector,
            MountSelector::Tag("app".to_owned())
        );
        assert!(manifest.entries()[0].writable);
    }

    #[test]
    fn accepts_block_selector_as_fallback() {
        let manifest = MountManifest::parse("/task=asset:blk0").unwrap();

        assert_eq!(
            manifest.entries()[0].selector,
            MountSelector::Block("blk0".to_owned())
        );
        assert!(!manifest.entries()[0].writable);
    }

    #[test]
    fn rejects_invalid_mount_shape() {
        assert_eq!(
            MountManifest::parse("task=asset:label=foo").unwrap_err(),
            MountManifestError::InvalidMountPath("task".to_owned())
        );
        assert_eq!(
            MountManifest::parse("/task=asset:uuid=bad").unwrap_err(),
            MountManifestError::InvalidUuid("bad".to_owned())
        );
        assert_eq!(
            MountManifest::parse("/task=asset:label=bad token").unwrap_err(),
            MountManifestError::InvalidToken {
                field: "label",
                value: "bad token".to_owned()
            }
        );
    }

    #[test]
    fn resolves_manifest_against_discovered_candidates() {
        let manifest = MountManifest::parse(
            "/task=asset:label=terminalbench-hello-world:ro,/repo=repo:uuid=00112233445566778899aabbccddeeff:rw",
        )
        .unwrap();
        let candidates = vec![
            MountCandidate {
                block_name: "blk0".to_owned(),
                kind: MountKind::Asset,
                uuid: Some("aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa".to_owned()),
                label: Some("terminalbench-hello-world".to_owned()),
                writable: false,
            },
            MountCandidate {
                block_name: "blk1".to_owned(),
                kind: MountKind::Repo,
                uuid: Some("00112233-4455-6677-8899-aabbccddeeff".to_owned()),
                label: Some("workspace-repo".to_owned()),
                writable: true,
            },
        ];

        let mounts = resolve_mount_plan(&manifest, &candidates).unwrap();

        assert_eq!(mounts.len(), 2);
        assert_eq!(mounts[0].mount_path, "/task");
        assert_eq!(mounts[0].block_name, "blk0");
        assert!(!mounts[0].writable);
        assert_eq!(mounts[1].mount_path, "/repo");
        assert_eq!(mounts[1].block_name, "blk1");
        assert!(mounts[1].writable);
    }

    #[test]
    fn resolves_block_selector_as_fallback() {
        let manifest = MountManifest::parse("/task=asset:block=blk7:ro").unwrap();
        let candidates = vec![MountCandidate {
            block_name: "blk7".to_owned(),
            kind: MountKind::Asset,
            uuid: None,
            label: None,
            writable: false,
        }];

        let mounts = resolve_mount_plan(&manifest, &candidates).unwrap();

        assert_eq!(mounts[0].block_name, "blk7");
    }

    #[test]
    fn reports_unresolved_ambiguous_and_readonly_mounts() {
        let missing = MountManifest::parse("/task=asset:label=missing").unwrap();
        assert!(matches!(
            resolve_mount_plan(&missing, &[]),
            Err(MountResolveError::NotFound(_))
        ));

        let ambiguous = MountManifest::parse("/task=asset:label=same").unwrap();
        let candidates = vec![
            MountCandidate {
                block_name: "blk0".to_owned(),
                kind: MountKind::Asset,
                uuid: None,
                label: Some("same".to_owned()),
                writable: false,
            },
            MountCandidate {
                block_name: "blk1".to_owned(),
                kind: MountKind::Asset,
                uuid: None,
                label: Some("same".to_owned()),
                writable: false,
            },
        ];
        assert!(matches!(
            resolve_mount_plan(&ambiguous, &candidates),
            Err(MountResolveError::Ambiguous(_))
        ));

        let readonly = MountManifest::parse("/repo=repo:block=blk2:rw").unwrap();
        let candidates = vec![MountCandidate {
            block_name: "blk2".to_owned(),
            kind: MountKind::Repo,
            uuid: None,
            label: None,
            writable: false,
        }];
        assert!(matches!(
            resolve_mount_plan(&readonly, &candidates),
            Err(MountResolveError::ReadOnlyCandidate { .. })
        ));
    }
}
