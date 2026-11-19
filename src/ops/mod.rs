// File operations: small asynchronous functions on top of GIO, so they work
// for any location GIO can reach (local folders, network shares) and never
// freeze the window. Each one only does the work; telling the user about the
// result is up to the caller.
// SPDX-License-Identifier: GPL-3.0-or-later
//
// Restoring from the trash follows Nautilus (GPL-3.0-or-later): look through
// trash:/// for entries whose original path and deletion time match the files
// we trashed, and move those back.

use std::future::Future;
use std::pin::Pin;

use gtk::prelude::*;
use gtk::{gio, glib};

const PRIORITY: glib::Priority = glib::Priority::DEFAULT;

/// Creates the folder `name` inside `parent` and returns it.
pub async fn make_folder(parent: &gio::File, name: &str) -> Result<gio::File, glib::Error> {
    let folder = parent.child(name);
    folder.make_directory_future(PRIORITY).await?;
    Ok(folder)
}

/// Gives `file` the new name `name` and returns the renamed file.
pub async fn rename(file: &gio::File, name: &str) -> Result<gio::File, glib::Error> {
    file.set_display_name_future(name, PRIORITY).await
}

/// A file that went to the trash, and when (seconds since 1970), so that
/// exactly this entry can be found again in the trash.
#[derive(Clone, Debug)]
pub struct Trashed {
    pub original: gio::File,
    pub time: i64,
}

/// What became of the files given to [`trash`].
#[derive(Debug, Default)]
pub struct TrashOutcome {
    pub trashed: Vec<Trashed>,
    /// Files whose location has no trash (GIO says "not supported").
    pub unsupported: Vec<gio::File>,
    /// Files that could not be trashed for another reason.
    pub failed: Vec<(gio::File, glib::Error)>,
}

/// Whether `files` can go to the trash, as far as their location says in
/// advance: false only if every one of them is known not to (a location
/// without a trash). Locations that don't say are given a try.
pub async fn can_trash(files: &[gio::File]) -> bool {
    for file in files {
        let info = file
            .query_info_future(
                gio::FILE_ATTRIBUTE_ACCESS_CAN_TRASH,
                gio::FileQueryInfoFlags::NOFOLLOW_SYMLINKS,
                PRIORITY,
            )
            .await;
        let known_not = info.is_ok_and(|info| {
            info.has_attribute(gio::FILE_ATTRIBUTE_ACCESS_CAN_TRASH)
                && !info.boolean(gio::FILE_ATTRIBUTE_ACCESS_CAN_TRASH)
        });
        if !known_not {
            return true;
        }
    }
    false
}

/// Moves `files` to the trash, one after the other.
pub async fn trash(files: &[gio::File]) -> TrashOutcome {
    let mut outcome = TrashOutcome::default();
    for file in files {
        let time = now();
        match file.trash_future(PRIORITY).await {
            Ok(()) => outcome.trashed.push(Trashed { original: file.clone(), time }),
            Err(error) if error.matches(gio::IOErrorEnum::NotSupported) => {
                outcome.unsupported.push(file.clone())
            }
            Err(error) => outcome.failed.push((file.clone(), error)),
        }
    }
    outcome
}

/// Moves the files of a [`trash`] call back to where they were. Returns the
/// restored files and the files that could not be restored (with why).
pub async fn restore(
    trashed: &[Trashed],
) -> Result<(Vec<gio::File>, Vec<(gio::File, glib::Error)>), glib::Error> {
    let trash = gio::File::for_uri("trash:///");
    let attributes = format!(
        "standard::name,{},{}",
        gio::FILE_ATTRIBUTE_TRASH_ORIG_PATH,
        gio::FILE_ATTRIBUTE_TRASH_DELETION_DATE
    );
    let entries = trash
        .enumerate_children_future(&attributes, gio::FileQueryInfoFlags::NONE, PRIORITY)
        .await?;

    // For every trashed file, the trash entry closest to its trashing time
    // (within a few seconds, as in Nautilus): the same path may have been
    // trashed before.
    let mut best: Vec<Option<(i64, gio::File)>> = vec![None; trashed.len()];
    loop {
        let infos = entries.next_files_future(64, PRIORITY).await?;
        if infos.is_empty() {
            break;
        }
        for info in infos {
            let Some(orig_path) = info.attribute_byte_string(gio::FILE_ATTRIBUTE_TRASH_ORIG_PATH)
            else {
                continue;
            };
            let Some(deleted) = info.deletion_date().map(|date| date.to_unix()) else {
                continue;
            };
            let original = gio::File::for_path(orig_path.as_str());
            for (i, item) in trashed.iter().enumerate() {
                let distance = (deleted - item.time).abs();
                if distance <= 2
                    && item.original.equal(&original)
                    && best[i].as_ref().is_none_or(|(d, _)| distance < *d)
                {
                    best[i] = Some((distance, trash.child(info.name())));
                }
            }
        }
    }
    entries.close_future(PRIORITY).await?;

    let mut restored = Vec::new();
    let mut failed = Vec::new();
    for (item, found) in trashed.iter().zip(best) {
        let Some((_, entry)) = found else {
            let error = glib::Error::new(gio::IOErrorEnum::NotFound, "No longer in the trash");
            failed.push((item.original.clone(), error));
            continue;
        };
        let (moved, _progress) =
            entry.move_future(&item.original, gio::FileCopyFlags::NOFOLLOW_SYMLINKS, PRIORITY);
        match moved.await {
            Ok(()) => restored.push(item.original.clone()),
            Err(error) => failed.push((item.original.clone(), error)),
        }
    }
    Ok((restored, failed))
}

/// Deletes `file` for good; a folder together with everything in it.
/// Symbolic links are deleted themselves, never followed.
pub fn delete_recursively(
    file: &gio::File,
) -> Pin<Box<dyn Future<Output = Result<(), glib::Error>> + '_>> {
    Box::pin(async move {
        let info = file
            .query_info_future(
                "standard::type",
                gio::FileQueryInfoFlags::NOFOLLOW_SYMLINKS,
                PRIORITY,
            )
            .await?;
        if info.file_type() == gio::FileType::Directory {
            let entries = file
                .enumerate_children_future(
                    "standard::name",
                    gio::FileQueryInfoFlags::NOFOLLOW_SYMLINKS,
                    PRIORITY,
                )
                .await?;
            loop {
                let infos = entries.next_files_future(64, PRIORITY).await?;
                if infos.is_empty() {
                    break;
                }
                for info in infos {
                    delete_recursively(&file.child(info.name())).await?;
                }
            }
            entries.close_future(PRIORITY).await?;
        }
        file.delete_future(PRIORITY).await
    })
}

fn now() -> i64 {
    glib::real_time() / 1_000_000
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::{Path, PathBuf};

    /// A fresh, empty folder for one test, removed again by `cleanup`.
    fn temp_folder(name: &str) -> PathBuf {
        let path =
            std::env::temp_dir().join(format!("kommander-test-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).unwrap();
        path
    }

    fn cleanup(path: &Path) {
        let _ = std::fs::remove_dir_all(path);
    }

    /// Runs `future` to the end on a GLib main context of its own.
    fn block_on<T>(future: impl Future<Output = T>) -> T {
        let context = glib::MainContext::new();
        context.with_thread_default(|| context.block_on(future)).unwrap()
    }

    #[test]
    fn make_folder_and_rename() {
        let dir = temp_folder("mkdir");
        let parent = gio::File::for_path(&dir);
        let folder = block_on(make_folder(&parent, "New")).unwrap();
        assert!(dir.join("New").is_dir());
        // A folder of that name exists now.
        let error = block_on(make_folder(&parent, "New")).unwrap_err();
        assert!(error.matches(gio::IOErrorEnum::Exists));

        let renamed = block_on(rename(&folder, "Renamed")).unwrap();
        assert_eq!(renamed.path().unwrap(), dir.join("Renamed"));
        assert!(!dir.join("New").exists());
        cleanup(&dir);
    }

    #[test]
    fn can_trash_gives_unknown_locations_a_try() {
        let dir = temp_folder("can-trash");
        // A file that is not there says nothing about its location's trash.
        let missing = gio::File::for_path(dir.join("missing"));
        assert!(block_on(can_trash(std::slice::from_ref(&missing))));
        // Whether the temporary folder has a trash depends on the machine;
        // the answer only has to match what GIO says about it.
        let file = gio::File::for_path(&dir);
        let says = file
            .query_info(
                gio::FILE_ATTRIBUTE_ACCESS_CAN_TRASH,
                gio::FileQueryInfoFlags::NONE,
                gio::Cancellable::NONE,
            )
            .unwrap()
            .boolean(gio::FILE_ATTRIBUTE_ACCESS_CAN_TRASH);
        assert_eq!(block_on(can_trash(std::slice::from_ref(&file))), says);
        assert!(!block_on(can_trash(&[])));
        cleanup(&dir);
    }

    #[test]
    fn delete_recursively_removes_contents_but_not_link_targets() {
        let dir = temp_folder("delete");
        let outside = dir.join("outside");
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("keep.txt"), "keep").unwrap();

        let doomed = dir.join("doomed");
        std::fs::create_dir_all(doomed.join("sub/deeper")).unwrap();
        std::fs::write(doomed.join("a.txt"), "a").unwrap();
        std::fs::write(doomed.join("sub/deeper/b.txt"), "b").unwrap();
        std::os::unix::fs::symlink(&outside, doomed.join("link")).unwrap();

        block_on(delete_recursively(&gio::File::for_path(&doomed))).unwrap();
        assert!(!doomed.exists());
        assert!(outside.join("keep.txt").exists());

        // A single file works too; a missing one is an error.
        let file = gio::File::for_path(outside.join("keep.txt"));
        block_on(delete_recursively(&file)).unwrap();
        assert!(block_on(delete_recursively(&file)).is_err());
        cleanup(&dir);
    }
}
