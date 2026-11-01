//! What a backup's destination may be, decided before anything is written.

use std::ffi::OsStr;
use std::fs;
use std::io;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use conbus_core::paths::Paths;
use conbus_core::segment::DEFAULT_SEGMENT_BYTES;

use super::{Layout, Record, copy, named, refuse};
use crate::report::{gib, statvfs};
use conbus_core::segment::SegmentLog;

/// Cloud-synced folders, relative to the home directory. A run `stat`s these, on
/// every run, and never lists them: reading inside iCloud Drive can stall for
/// minutes or put a permission dialog on the person's screen.
const CLOUD_FOLDERS: &[&[&str]] = &[
    &["Library", "Mobile Documents"],
    &["Library", "CloudStorage"],
    &["Dropbox"],
    &["Google Drive"],
];

/// Cloud-synced folders named by how they start: `OneDrive - Acme`,
/// `Dropbox (Personal)`. Found by reading the names in the home directory.
const CLOUD_PREFIXES: &[&str] = &["OneDrive", "Dropbox ("];

/// Folders iCloud can sync, a setting conbus cannot see. Compared with the path
/// a person asked for and never `stat`ed: a metadata read of these two has stalled
/// on a real Mac, so a run for any other folder must not go near them.
const SYNCABLE_FOLDERS: &[&str] = &["Desktop", "Documents"];

/// What a run needs free beyond the bytes themselves. A clone still costs an
/// inode and its extent records: under 1 KiB each on an APFS volume with nothing
/// else writing, so this is margin. A copy can round each file up to a large
/// exFAT cluster. The manifest is rewritten whole. And on the log's own disk the
/// recorder needs room for what it writes next.
const CLONE_COST: u64 = 64 * 1024;
const COPY_SLACK: u64 = 256 * 1024;
const MANIFEST_ROW: u64 = 256;
const RESERVE: u64 = DEFAULT_SEGMENT_BYTES;

/// Every check that can be made without writing anything, on where the folder
/// really is, and again on anything already inside it, which could lead
/// somewhere else. Returns the folder as it really is.
pub(super) fn check(root: &Path, to: &Path, home: Option<&Path>) -> io::Result<PathBuf> {
    let Some(home) = home else {
        return Err(refuse(
            "no HOME to check the folder against the cloud-synced ones; set HOME",
        ));
    };
    // By name first, before anything looks at the folder.
    let spelled = std::path::absolute(to).unwrap_or_else(|_| to.to_path_buf());
    refuse_by_name(&spelled, home)?;
    let root = fs::canonicalize(root).map_err(named(root))?;
    let folder = resolve(to)?;
    refuse_location(&folder, &root, home)?;
    let layout = Layout::of(&folder);
    for inner in [&layout.dir, &layout.log] {
        if fs::symlink_metadata(inner).is_ok() {
            refuse_location(&resolve(inner)?, &root, home)?;
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

fn refuse_location(folder: &Path, root: &Path, home: &Path) -> io::Result<()> {
    if let Some(volume) = unmounted_volume(folder, &mount_holders()) {
        return Err(refuse(format!(
            "{} is not mounted, and conbus will not create a folder of that name on another \
             disk; plug the disk in and run this again",
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
    refuse_by_name(folder, home)?;
    match existing_cloud_folders(home)
        .into_iter()
        .find(|cloud| is_inside(folder, cloud))
    {
        Some(cloud) => Err(in_a_cloud_folder(folder, &cloud)),
        None => Ok(()),
    }
}

fn in_a_cloud_folder(folder: &Path, cloud: &Path) -> io::Error {
    refuse(format!(
        "{} is in a cloud-synced folder ({}). The log holds prompts and replies in plain text, \
         and conbus promises they do not leave this machine; encrypted cloud backup is not \
         offered yet. Choose a folder on a local or external disk",
        folder.display(),
        cloud.display()
    ))
}

/// What can be told from how a path is spelled, without looking at anything it
/// names. Works for a folder that does not exist yet: a client installed tomorrow
/// syncs what is already there.
fn refuse_by_name(path: &Path, home: &Path) -> io::Result<()> {
    let real_home = fs::canonicalize(home).unwrap_or_else(|_| home.to_path_buf());
    for home in [home, real_home.as_path()] {
        if let Some(cloud) = named_cloud_folder(path, home) {
            return Err(in_a_cloud_folder(path, &cloud));
        }
        if let Some(syncable) = SYNCABLE_FOLDERS
            .iter()
            .find_map(|name| spelled_under(path, home, &[name]))
        {
            return Err(refuse(format!(
                "{} is under {}. On a Mac iCloud can sync Desktop and Documents, conbus cannot \
                 see that setting, and the log holds prompts and replies in plain text, which \
                 conbus promises do not leave this machine. Choose another folder, such as {}",
                path.display(),
                syncable.display(),
                home.join("conbus-backup").display()
            )));
        }
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

/// Where this system mounts other disks. A folder made here with the disk
/// unplugged would sit on another disk under a name that looks like the right one.
#[cfg(target_os = "macos")]
fn mount_holders() -> Vec<PathBuf> {
    vec![PathBuf::from("/Volumes")]
}

/// Deepest first: under `/media/me` a disk is the next level down, not `me`.
#[cfg(not(target_os = "macos"))]
fn mount_holders() -> Vec<PathBuf> {
    let user = std::env::var_os("USER").or_else(|| std::env::var_os("LOGNAME"));
    user.iter()
        .flat_map(|user| {
            [
                Path::new("/run/media").join(user),
                Path::new("/media").join(user),
            ]
        })
        .chain([PathBuf::from("/media"), PathBuf::from("/mnt")])
        .collect()
}

/// The folder under a mount holder that `folder` depends on, when that is not a
/// disk: absent, a plain folder on the holder's own device (what a stale mount
/// point leaves), or the holder itself. Only the first holder `folder` is under
/// has a say.
fn unmounted_volume(folder: &Path, holders: &[PathBuf]) -> Option<PathBuf> {
    let holder = holders.iter().find(|holder| is_inside(folder, holder))?;
    let held = identity(holder)?;
    let mut below: Option<&Path> = None;
    for at in folder.ancestors() {
        if identity(at) == Some(held) {
            let Some(volume) = below else {
                return Some(at.to_path_buf());
            };
            let mounted = identity(volume).is_some_and(|(device, _)| device != held.0);
            return (!mounted).then(|| volume.to_path_buf());
        }
        below = Some(at);
    }
    None
}

fn has_cloud_prefix(name: &OsStr) -> bool {
    let Some(name) = name.to_str() else {
        return false;
    };
    CLOUD_PREFIXES.iter().any(|prefix| {
        name.get(..prefix.len())
            .is_some_and(|start| start.eq_ignore_ascii_case(prefix))
    })
}

fn under_home(home: &Path, folder: &[&str]) -> PathBuf {
    folder.iter().fold(home.to_path_buf(), |at, n| at.join(n))
}

/// `home/folder`, if `path` is that or anything under it, in any case: the
/// default Mac volume does not care about case.
fn spelled_under(path: &Path, home: &Path, folder: &[&str]) -> Option<PathBuf> {
    let parts: Vec<&OsStr> = path.strip_prefix(home).ok()?.iter().collect();
    let same = |part: &OsStr, name: &str| {
        part.to_str()
            .is_some_and(|part| part.eq_ignore_ascii_case(name))
    };
    (parts.len() >= folder.len() && folder.iter().zip(&parts).all(|(n, p)| same(p, n)))
        .then(|| under_home(home, folder))
}

/// The cloud folder `path` is spelled into, if any.
fn named_cloud_folder(path: &Path, home: &Path) -> Option<PathBuf> {
    let first = path.strip_prefix(home).ok()?.iter().next()?;
    if has_cloud_prefix(first) {
        return Some(home.join(first));
    }
    CLOUD_FOLDERS
        .iter()
        .find_map(|folder| spelled_under(path, home, folder))
}

/// The cloud folders present in this home, by whatever they point at: `~/Dropbox`
/// may be a link to another disk, and what is there still syncs.
fn existing_cloud_folders(home: &Path) -> Vec<PathBuf> {
    let fixed = CLOUD_FOLDERS.iter().map(|folder| under_home(home, folder));
    let prefixed = fs::read_dir(home)
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .filter(|entry| has_cloud_prefix(&entry.file_name()))
        .map(|entry| entry.path());
    fixed.chain(prefixed).collect()
}

/// `folder` if it is there yet, otherwise the parent `resolve` found.
pub(super) fn existing_folder(folder: &Path) -> &Path {
    if folder.exists() {
        folder
    } else {
        folder.parent().unwrap_or(folder)
    }
}

/// How the destination stands to the log.
pub(super) struct Standing {
    pub same_volume: bool,
    /// The same volume, or another volume in the same APFS container, which fails
    /// with it. Separate partitions of one physical disk do not show up here.
    pub same_disk: bool,
    pub network: bool,
    pub clones: bool,
}

pub(super) fn standing(log_dir: &Path, folder: &Path) -> io::Result<Standing> {
    let here = existing_folder(folder);
    let log = fs::metadata(log_dir).map_err(named(log_dir))?;
    let same_volume = log.dev() == fs::metadata(here).map_err(named(here))?.dev();
    let (log_volume, here_volume) = (copy::volume_of(log_dir), copy::volume_of(here));
    Ok(Standing {
        same_volume,
        same_disk: same_volume
            || (log_volume.disk.is_some() && log_volume.disk == here_volume.disk),
        network: here_volume.network,
        clones: same_volume && here_volume.clones,
    })
}

/// A mount folder outlives its disk (`/mnt/disk`, a stale `/Volumes/Name`), and
/// with the disk gone it is just a folder on some other disk. A run has finished
/// here before, and neither a manifest nor a copy is here now: copying would put
/// the whole log somewhere nobody chose. A run that was interrupted before its
/// first finish has no such claim, so it can go on.
pub(super) fn refuse_if_disk_went_away(
    paths: &Paths,
    record: &Record,
    folder: &Path,
    layout: &Layout,
) -> io::Result<()> {
    let name = folder.to_string_lossy();
    let expected = record
        .destinations
        .iter()
        .any(|known| known.finished && known.path == name);
    if expected
        && !layout.manifest.exists()
        && SegmentLog::segments(&layout.log)
            .map_err(named(&layout.log))?
            .is_empty()
    {
        return Err(refuse(format!(
            "{} held a backup at its last run and holds none now, so its disk is probably not \
             mounted. Mount it and run this again. If you removed that backup on purpose, take \
             this folder out of the destinations in {} first",
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

        let holders = [volumes.clone()];
        assert_eq!(
            unmounted_volume(&stale.join("backups"), &holders),
            Some(stale.clone())
        );
        assert_eq!(unmounted_volume(&stale, &holders), Some(stale.clone()));
        let absent = volumes.join("Unplugged");
        assert_eq!(unmounted_volume(&absent, &holders), Some(absent));
        assert_eq!(unmounted_volume(&volumes, &holders), Some(volumes.clone()));
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
        let holders = [root.to_path_buf()];
        assert_eq!(unmounted_volume(Path::new("/dev/fd"), &holders), None);
        assert_eq!(unmounted_volume(Path::new("/dev"), &holders), None);
    }

    #[test]
    fn a_folder_outside_the_mount_folder_is_none_of_its_business() {
        let temp = tempfile::tempdir().expect("tempdir");
        let base = temp.path().canonicalize().expect("canonical");
        let volumes = base.join("Volumes");
        fs::create_dir(&volumes).expect("dir");

        assert_eq!(
            unmounted_volume(&base.join("elsewhere"), std::slice::from_ref(&volumes)),
            None
        );
        assert_eq!(unmounted_volume(&base, &[base.join("no-such")]), None);
    }

    #[test]
    fn under_a_persons_own_mount_folder_the_disk_is_the_next_level_down() {
        // `/media/me` is a holder as well as a folder under `/media`, and it is
        // the deeper one that decides, or every mounted disk there would be
        // refused for `me` not being a disk.
        let temp = tempfile::tempdir().expect("tempdir");
        let base = temp.path().canonicalize().expect("canonical");
        let (media, mine) = (base.join("media"), base.join("media").join("me"));
        fs::create_dir_all(&mine).expect("dirs");
        let holders = [mine.clone(), media.clone()];

        let stale = mine.join("Disk");
        fs::create_dir(&stale).expect("dir");
        assert_eq!(
            unmounted_volume(&stale.join("backups"), &holders),
            Some(stale.clone())
        );
        let someone_elses = media.join("them");
        assert_eq!(
            unmounted_volume(&someone_elses.join("Disk"), &holders),
            Some(someone_elses),
            "with no holder deeper, the level under /media is what must be a disk"
        );
        assert_eq!(unmounted_volume(&mine, &holders), Some(mine.clone()));
        assert_eq!(unmounted_volume(&base.join("elsewhere"), &holders), None);
    }

    #[test]
    fn a_cloud_folder_named_by_how_it_starts() {
        for name in ["OneDrive", "OneDrive - Acme", "onedrive", "Dropbox (Acme)"] {
            assert!(has_cloud_prefix(OsStr::new(name)), "{name}");
        }
        for name in ["Dropbox", "Dropboxes", "Dropbox-notes", "Drive", "하나"] {
            assert!(!has_cloud_prefix(OsStr::new(name)), "{name}");
        }
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
