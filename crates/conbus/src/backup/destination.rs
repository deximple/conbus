//! What a backup's destination may be, decided before anything is written.

use std::ffi::OsStr;
use std::fs;
use std::io;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use conbus_core::paths::Paths;
use conbus_core::segment::DEFAULT_SEGMENT_BYTES;

use super::{Layout, named, read_record, refuse};
use crate::report::{gib, statvfs};

/// Where macOS mounts other disks. A folder made here with the disk unplugged
/// would sit on the internal disk under a name that looks external.
const VOLUMES: &str = "/Volumes";

/// Cloud-synced folders, relative to the home directory. `OneDrive*` is a prefix
/// and has its own check. Only ever `stat`ed, never listed: reading inside iCloud
/// Drive can stall for minutes or put a permission dialog on the person's screen.
const CLOUD_FOLDERS: &[&[&str]] = &[
    &["Library", "Mobile Documents"],
    &["Library", "CloudStorage"],
    &["Dropbox"],
    &["Google Drive"],
];
const ONEDRIVE: &str = "OneDrive";

/// What a run needs free beyond the bytes themselves. A clone still costs an
/// inode and its extent records: 1,468 clones of the real log moved `df` by about
/// 28 MiB, against noise of the same size. A copy can round each file up to a
/// large exFAT cluster. The manifest is rewritten whole. And on the log's own
/// disk the recorder needs room for what it writes next.
const CLONE_COST: u64 = 64 * 1024;
const COPY_SLACK: u64 = 256 * 1024;
const MANIFEST_ROW: u64 = 256;
const RESERVE: u64 = DEFAULT_SEGMENT_BYTES;

/// Every check that can be made without writing anything, on where the folder
/// really is, and again on anything already inside it, which could lead
/// somewhere else. Returns the folder as it really is.
pub(super) fn check(root: &Path, to: &Path, home: Option<&Path>) -> io::Result<PathBuf> {
    let root = fs::canonicalize(root).map_err(named(root))?;
    let folder = resolve(to)?;
    let spelled = std::path::absolute(to).unwrap_or_else(|_| to.to_path_buf());
    refuse_location(&folder, &spelled, &root, home)?;
    let layout = Layout::of(&folder);
    for inner in [&layout.dir, &layout.log] {
        if fs::symlink_metadata(inner).is_ok() {
            refuse_location(&resolve(inner)?, inner, &root, home)?;
        }
    }
    Ok(folder)
}

/// Where `to` really is, following links: itself if it is there, otherwise its
/// real parent joined to its name. The parent has to exist. Nothing here creates
/// a level of the path, because a missing level is what an unplugged disk looks
/// like.
fn resolve(to: &Path) -> io::Result<PathBuf> {
    match fs::canonicalize(to) {
        Ok(found) if found.is_dir() => Ok(found),
        Ok(found) => Err(refuse(format!("{} is not a folder", found.display()))),
        Err(err) if err.kind() == io::ErrorKind::NotFound => resolve_new(to),
        Err(err) => Err(refuse(format!("{}: {err}", to.display()))),
    }
}

fn resolve_new(to: &Path) -> io::Result<PathBuf> {
    if fs::symlink_metadata(to).is_ok() {
        return Err(refuse(format!(
            "{} is a link to something not there; if that is on a disk, the disk may be \
             unplugged",
            to.display()
        )));
    }
    let name = to
        .file_name()
        .ok_or_else(|| refuse(format!("{} does not name a folder", to.display())))?;
    let parent = match to.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent,
        _ => Path::new("."),
    };
    let real = fs::canonicalize(parent).map_err(|err| {
        if err.kind() == io::ErrorKind::NotFound {
            refuse(format!(
                "{} does not exist, so {} cannot be made there; if it is on a disk, the disk \
                 may be unplugged, and conbus will not create it",
                parent.display(),
                to.display()
            ))
        } else {
            refuse(format!("{}: {err}", parent.display()))
        }
    })?;
    if !real.is_dir() {
        return Err(refuse(format!("{} is not a folder", real.display())));
    }
    Ok(real.join(name))
}

fn refuse_location(
    folder: &Path,
    spelled: &Path,
    root: &Path,
    home: Option<&Path>,
) -> io::Result<()> {
    if let Some(volume) = unmounted_volume(folder, Path::new(VOLUMES)) {
        return Err(refuse(format!(
            "{} is not mounted, and conbus will not create a folder of that name on the \
             internal disk; plug the disk in and run this again",
            volume.display()
        )));
    }
    if is_inside(folder, root) {
        return Err(refuse(format!(
            "{} is inside the data root {}: a copy there dies with the log",
            folder.display(),
            root.display()
        )));
    }
    if is_inside(root, folder) {
        return Err(refuse(format!(
            "{} holds the data root {}: a backup there would contain the log it copies",
            folder.display(),
            root.display()
        )));
    }
    let Some(home) = home else {
        return Err(refuse(
            "no HOME to check the folder against the cloud-synced ones; set HOME",
        ));
    };
    if let Some(cloud) = cloud_folder(folder, spelled, home) {
        return Err(refuse(format!(
            "{} is in a cloud-synced folder ({}). The log holds prompts and replies in plain \
             text, and conbus promises they do not leave this machine; encrypted cloud backup \
             is not offered yet. Choose a folder on a local or external disk",
            folder.display(),
            cloud.display()
        )));
    }
    Ok(())
}

/// What `path` is, to the file system. Spelling is not good enough: the default
/// Mac volume ignores case, and a firmlink gives one folder two names.
fn identity(path: &Path) -> Option<(u64, u64)> {
    let meta = fs::metadata(path).ok()?;
    Some((meta.dev(), meta.ino()))
}

fn is_inside(inner: &Path, outer: &Path) -> bool {
    let Some(outer) = identity(outer) else {
        return false;
    };
    inner.ancestors().any(|at| identity(at) == Some(outer))
}

/// The folder under `volumes` that `folder` depends on, when that is not a disk:
/// absent, a plain folder on the same device as `volumes` (what a stale mount
/// point leaves), or `volumes` itself.
fn unmounted_volume(folder: &Path, volumes: &Path) -> Option<PathBuf> {
    let holder = identity(volumes)?;
    let mut below: Option<&Path> = None;
    for at in folder.ancestors() {
        if identity(at) == Some(holder) {
            let Some(volume) = below else {
                return Some(at.to_path_buf());
            };
            let mounted = identity(volume).is_some_and(|(device, _)| device != holder.0);
            return (!mounted).then(|| volume.to_path_buf());
        }
        below = Some(at);
    }
    None
}

fn is_onedrive(name: &OsStr) -> bool {
    name.to_str()
        .and_then(|name| name.get(..ONEDRIVE.len()))
        .is_some_and(|start| start.eq_ignore_ascii_case(ONEDRIVE))
}

fn under_home(home: &Path, folder: &[&str]) -> PathBuf {
    folder.iter().fold(home.to_path_buf(), |at, n| at.join(n))
}

/// The cloud folder `path` is spelled into, if any. Works for a folder that does
/// not exist yet: a client installed tomorrow syncs what is already there.
fn named_cloud_folder(path: &Path, home: &Path) -> Option<PathBuf> {
    let parts: Vec<&OsStr> = path.strip_prefix(home).ok()?.iter().collect();
    if is_onedrive(parts.first()?) {
        return Some(home.join(parts[0]));
    }
    let same = |part: &OsStr, name: &str| {
        part.to_str()
            .is_some_and(|part| part.eq_ignore_ascii_case(name))
    };
    CLOUD_FOLDERS
        .iter()
        .find(|folder| {
            parts.len() >= folder.len() && folder.iter().zip(&parts).all(|(n, p)| same(p, n))
        })
        .map(|folder| under_home(home, folder))
}

/// The cloud folders present in this home, by whatever they point at: `~/Dropbox`
/// may be a link to another disk, and what is there still syncs.
fn existing_cloud_folders(home: &Path) -> Vec<PathBuf> {
    let fixed = CLOUD_FOLDERS.iter().map(|folder| under_home(home, folder));
    let onedrive = fs::read_dir(home)
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .filter(|entry| is_onedrive(&entry.file_name()))
        .map(|entry| entry.path());
    fixed.chain(onedrive).collect()
}

fn cloud_folder(folder: &Path, spelled: &Path, home: &Path) -> Option<PathBuf> {
    let real_home = fs::canonicalize(home).unwrap_or_else(|_| home.to_path_buf());
    for path in [folder, spelled] {
        for home in [home, real_home.as_path()] {
            if let Some(found) = named_cloud_folder(path, home) {
                return Some(found);
            }
        }
    }
    existing_cloud_folders(home)
        .into_iter()
        .find(|cloud| is_inside(folder, cloud))
}

/// `folder` if it is there yet, otherwise the parent `resolve` found.
pub(super) fn existing_folder(folder: &Path) -> &Path {
    if folder.exists() {
        folder
    } else {
        folder.parent().unwrap_or(folder)
    }
}

pub(super) fn on_log_volume(log_dir: &Path, folder: &Path) -> io::Result<bool> {
    let here = existing_folder(folder);
    let log = fs::metadata(log_dir).map_err(named(log_dir))?;
    Ok(log.dev() == fs::metadata(here).map_err(named(here))?.dev())
}

/// A mount folder outlives its disk (`/mnt/disk`, a stale `/Volumes/Name`), and
/// with the disk gone it is just a folder on the log's own disk. Only the record
/// can tell that apart from a person choosing that folder.
pub(super) fn refuse_if_disk_went_away(
    paths: &Paths,
    folder: &Path,
    same_volume: bool,
) -> io::Result<()> {
    let Ok(Some(last)) = read_record(paths) else {
        return Ok(());
    };
    if same_volume && !last.same_volume && last.destination == folder.to_string_lossy() {
        return Err(refuse(format!(
            "{} was on another disk at the last backup and is on the log's disk now, so that \
             disk is probably not mounted. Mount it and run this again; if the folder really \
             moved here, delete {} first",
            folder.display(),
            paths.backup_record().display()
        )));
    }
    Ok(())
}

fn space_needed(bytes: u64, files: u64, rows: u64, clones: bool) -> u64 {
    let data = if clones {
        files * CLONE_COST
    } else {
        bytes + files * COPY_SLACK
    };
    data + (rows + files) * MANIFEST_ROW + RESERVE
}

/// Refuse a run that cannot fit, before it writes anything. A run with nothing to
/// copy is let through, however full the disk: it only rewrites a small file.
pub(super) fn refuse_if_full(
    folder: &Path,
    bytes: u64,
    files: u64,
    rows: u64,
    clones: bool,
) -> io::Result<()> {
    if files == 0 {
        return Ok(());
    }
    let needed = space_needed(bytes, files, rows, clones);
    let here = existing_folder(folder);
    let (free, _) = statvfs(here).ok_or_else(|| named(here)(io::Error::last_os_error()))?;
    if free < needed {
        return Err(refuse(format!(
            "not enough free space on {}: this run needs about {} and {} is free. Nothing was \
             copied",
            here.display(),
            gib(needed),
            gib(free)
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_plain_folder_where_a_disk_should_be_is_not_a_disk() {
        // A stale mount point: `/Volumes/Name` with no disk behind it.
        let temp = tempfile::tempdir().expect("tempdir");
        let volumes = temp.path().canonicalize().expect("canonical");
        let stale = volumes.join("My Disk");
        fs::create_dir(&stale).expect("dir");

        assert_eq!(
            unmounted_volume(&stale.join("backups"), &volumes),
            Some(stale.clone())
        );
        assert_eq!(unmounted_volume(&stale, &volumes), Some(stale.clone()));
        let absent = volumes.join("Unplugged");
        assert_eq!(unmounted_volume(&absent, &volumes), Some(absent));
        assert_eq!(unmounted_volume(&volumes, &volumes), Some(volumes.clone()));
    }

    #[test]
    fn a_folder_on_its_own_device_is_a_disk() {
        // `/dev` is its own file system on both macOS and Linux, so this is a
        // real mount point without having to make one.
        let root = Path::new("/");
        assert_ne!(
            identity(Path::new("/dev")).map(|id| id.0),
            identity(root).map(|id| id.0)
        );
        assert_eq!(unmounted_volume(Path::new("/dev/fd"), root), None);
        assert_eq!(unmounted_volume(Path::new("/dev"), root), None);
    }

    #[test]
    fn a_folder_outside_the_mount_folder_is_none_of_its_business() {
        let temp = tempfile::tempdir().expect("tempdir");
        let base = temp.path().canonicalize().expect("canonical");
        let volumes = base.join("Volumes");
        fs::create_dir(&volumes).expect("dir");

        assert_eq!(unmounted_volume(&base.join("elsewhere"), &volumes), None);
        assert_eq!(unmounted_volume(&base, &base.join("no-such")), None);
    }

    #[test]
    fn a_clone_needs_room_for_its_records_and_a_copy_for_its_bytes() {
        let gb = 1 << 30;
        let clone = space_needed(10 * gb, 1_460, 1_460, true);
        let copy = space_needed(10 * gb, 1_460, 1_460, false);
        assert!(clone < gb, "{clone}");
        assert!(copy > 10 * gb, "{copy}");
    }
}
