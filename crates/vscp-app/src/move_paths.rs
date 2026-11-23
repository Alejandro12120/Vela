use std::{
    collections::BTreeSet,
    io,
    path::{Path, PathBuf},
};

use gpui::BackgroundExecutor;
use vscp_core::{
    Entry, Error, local,
    transfer::{Choice, Conflict, numbered_name},
};

use crate::pane::Source;

pub struct Outcome {
    pub parents: BTreeSet<PathBuf>,
    pub moved: Vec<(PathBuf, PathBuf)>,
    pub errors: Vec<String>,
}

pub async fn run(
    source: Source,
    mut sources: Vec<PathBuf>,
    folder: PathBuf,
    executor: &BackgroundExecutor,
    mut ask: impl FnMut(Conflict, bool) -> bool,
) -> Outcome {
    sources.sort();
    let mut paths: Vec<PathBuf> = Vec::new();
    for path in sources {
        // Paths sort by components, so descendants are consecutive.
        if !paths.last().is_some_and(|parent| path.starts_with(parent)) {
            paths.push(path);
        }
    }
    let mut result = Outcome {
        parents: paths
            .iter()
            .filter_map(|path| path.parent().map(Path::to_owned))
            .chain(std::iter::once(folder.clone()))
            .collect(),
        moved: Vec::new(),
        errors: Vec::new(),
    };
    let mut policy = None;
    for path in &paths {
        let Some(name) = path.file_name() else {
            continue;
        };
        let dest = folder.join(name);
        if dest == *path {
            continue;
        }
        let attempt = source.rename(path.clone(), dest.clone(), executor).await;
        let attempt = match attempt {
            Ok(()) => Ok(Some(dest)),
            Err(error) if already_exists(&error) => {
                let conflict = async {
                    let entry = stat(&source, path.clone(), executor).await?;
                    let existing = stat(&source, dest.clone(), executor).await?;
                    let allow_overwrite = allow_overwrite(&source, &paths, &dest, executor).await?;
                    let choice = match policy
                        .filter(|choice| *choice != Choice::Overwrite || allow_overwrite)
                    {
                        Some(choice) => choice,
                        None => {
                            let (conflict, answer) = Conflict::new(dest.clone(), entry, existing);
                            if !ask(conflict, allow_overwrite) {
                                return Err(Error::Cancelled);
                            }
                            let answer = answer.recv().await.map_err(|_| Error::Cancelled)?;
                            if answer.apply_to_all {
                                policy = Some(answer.choice);
                            }
                            answer.choice
                        }
                    };
                    match choice {
                        Choice::Cancel => Err(Error::Cancelled),
                        Choice::Skip => Ok(None),
                        Choice::Rename => {
                            for n in 2.. {
                                let numbered = folder.join(numbered_name(name, n));
                                match source
                                    .rename(path.clone(), numbered.clone(), executor)
                                    .await
                                {
                                    Ok(()) => return Ok(Some(numbered)),
                                    Err(error) if already_exists(&error) => {}
                                    Err(error) => return Err(error),
                                }
                            }
                            unreachable!()
                        }
                        Choice::Overwrite => {
                            if !allow_overwrite
                                || !self::allow_overwrite(&source, &paths, &dest, executor).await?
                            {
                                return Err(io::Error::other(
                                    "El destino contiene un elemento que se va a mover",
                                )
                                .into());
                            }
                            // The source may have disappeared while the dialog was open.
                            stat(&source, path.clone(), executor).await?;
                            remove_destination(&source, dest.clone(), executor).await?;
                            source.rename(path.clone(), dest.clone(), executor).await?;
                            Ok(Some(dest))
                        }
                    }
                };
                conflict.await
            }
            Err(error) => Err(error),
        };
        match attempt {
            Ok(Some(dest)) => result.moved.push((path.clone(), dest)),
            Ok(None) => {}
            Err(Error::Cancelled) => break,
            Err(error) => result.errors.push(format!("{} · {error}", path.display())),
        }
    }
    result
}

fn already_exists(error: &Error) -> bool {
    matches!(error, Error::Io(error) if error.kind() == io::ErrorKind::AlreadyExists)
}

async fn stat(
    source: &Source,
    path: PathBuf,
    executor: &BackgroundExecutor,
) -> Result<Entry, Error> {
    match source {
        Source::Local => executor.spawn(async move { Ok(local::stat(&path)?) }).await,
        Source::Remote(session) => session.stat(path).await,
    }
}

async fn allow_overwrite(
    source: &Source,
    paths: &[PathBuf],
    existing: &Path,
    executor: &BackgroundExecutor,
) -> Result<bool, Error> {
    if paths.iter().any(|path| path.starts_with(existing)) {
        return Ok(false);
    }
    // The tree can expand a symlink to a directory. Resolve parent components
    // so that a different spelling of the same source cannot bypass the guard.
    let existing = real_parent(source, existing, executor).await?;
    for path in paths {
        if real_parent(source, path, executor)
            .await?
            .starts_with(&existing)
        {
            return Ok(false);
        }
    }
    Ok(true)
}

async fn real_parent(
    source: &Source,
    path: &Path,
    executor: &BackgroundExecutor,
) -> Result<PathBuf, Error> {
    let (Some(parent), Some(name)) = (path.parent(), path.file_name()) else {
        return Ok(path.to_owned());
    };
    let parent = parent.to_owned();
    let real = match source {
        Source::Local => executor.spawn(async move { parent.canonicalize() }).await?,
        Source::Remote(session) => session.realpath(parent).await?,
    };
    Ok(real.join(name))
}

async fn remove_destination(
    source: &Source,
    path: PathBuf,
    executor: &BackgroundExecutor,
) -> Result<(), Error> {
    if cfg!(target_os = "macos") || matches!(source, Source::Remote(_)) {
        return source.delete(path, executor).await;
    }
    executor
        .spawn(async move {
            if std::fs::symlink_metadata(&path)?.is_dir() {
                std::fs::remove_dir_all(&path)?;
            } else {
                std::fs::remove_file(&path)?;
            }
            Ok(())
        })
        .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::TestAppContext;
    use std::fs;
    use vscp_core::transfer::Answer;

    #[gpui::test]
    async fn local_moves_to_folder_root_and_sibling(cx: &mut TestAppContext) {
        let fixture = tempfile::tempdir().unwrap();
        let root = fixture.path();
        fs::create_dir_all(root.join("a/child")).unwrap();
        fs::create_dir(root.join("b")).unwrap();
        fs::write(root.join("file.txt"), "file").unwrap();
        fs::write(root.join("a/child/inside.txt"), "inside").unwrap();
        let executor = cx.executor();
        for (from, folder, to) in [
            ("file.txt", "a", "a/file.txt"),
            ("a/child", "", "child"),
            ("child", "b", "b/child"),
        ] {
            let from = root.join(from);
            let folder = root.join(folder);
            let to = root.join(to);
            let sources = if from.is_dir() {
                vec![from.join("inside.txt"), from.clone()]
            } else {
                vec![from.clone()]
            };
            let result = run(Source::Local, sources, folder, &executor, |_, _| {
                panic!("unexpected conflict");
            })
            .await;
            assert!(result.errors.is_empty(), "{:?}", result.errors);
            assert_eq!(result.moved, [(from.clone(), to.clone())]);
            assert!(!from.exists());
            assert!(to.exists());
        }
        assert_eq!(
            fs::read_to_string(root.join("b/child/inside.txt")).unwrap(),
            "inside"
        );
    }

    #[gpui::test]
    async fn skips_numbers_and_cancels_local_conflicts(cx: &mut TestAppContext) {
        for choice in [Choice::Skip, Choice::Rename, Choice::Cancel] {
            let fixture = tempfile::tempdir().unwrap();
            let root = fixture.path();
            fs::create_dir(root.join("dest")).unwrap();
            for name in ["0.txt", "a.txt", "b.txt"] {
                fs::write(root.join(name), "new").unwrap();
            }
            for name in ["a.txt", "b.txt"] {
                fs::write(root.join("dest").join(name), "existing").unwrap();
            }
            let mut prompts = 0;
            let result = run(
                Source::Local,
                ["0.txt", "a.txt", "b.txt"]
                    .map(|path| root.join(path))
                    .to_vec(),
                root.join("dest"),
                &cx.executor(),
                |conflict, allow_overwrite| {
                    assert!(allow_overwrite);
                    prompts += 1;
                    conflict.answer(Answer {
                        choice,
                        apply_to_all: true,
                    });
                    true
                },
            )
            .await;
            assert!(result.errors.is_empty());
            assert_eq!(prompts, 1);
            assert_eq!(
                fs::read_to_string(root.join("dest/a.txt")).unwrap(),
                "existing"
            );
            assert!(root.join("dest/0.txt").is_file());
            if choice == Choice::Rename {
                assert_eq!(
                    fs::read_to_string(root.join("dest/a (2).txt")).unwrap(),
                    "new"
                );
                assert_eq!(
                    fs::read_to_string(root.join("dest/b (2).txt")).unwrap(),
                    "new"
                );
            } else {
                assert!(root.join("a.txt").is_file());
                assert!(root.join("b.txt").is_file());
                assert_eq!(result.moved.len(), 1);
            }
        }
    }

    #[cfg(target_os = "linux")]
    #[gpui::test]
    async fn overwrites_a_local_directory(cx: &mut TestAppContext) {
        let fixture = tempfile::tempdir().unwrap();
        let root = fixture.path();
        fs::create_dir_all(root.join("dest/a")).unwrap();
        fs::write(root.join("dest/a/old.txt"), "old").unwrap();
        fs::create_dir(root.join("a")).unwrap();
        fs::write(root.join("a/new.txt"), "new").unwrap();
        let result = run(
            Source::Local,
            vec![root.join("a")],
            root.join("dest"),
            &cx.executor(),
            |conflict, allow_overwrite| {
                assert!(allow_overwrite);
                conflict.answer(Answer {
                    choice: Choice::Overwrite,
                    apply_to_all: false,
                });
                true
            },
        )
        .await;
        assert!(result.errors.is_empty());
        assert!(!root.join("dest/a/old.txt").exists());
        assert!(root.join("dest/a/new.txt").is_file());
        assert!(!root.join("a").exists());
    }

    #[gpui::test]
    async fn never_overwrites_a_source_ancestor(cx: &mut TestAppContext) {
        let fixture = tempfile::tempdir().unwrap();
        let root = fixture.path();
        fs::create_dir_all(root.join("a/a")).unwrap();
        fs::write(root.join("a/a/keep.txt"), "keep").unwrap();
        let result = run(
            Source::Local,
            vec![root.join("a/a")],
            root.to_owned(),
            &cx.executor(),
            |conflict, allow_overwrite| {
                assert!(!allow_overwrite);
                conflict.answer(Answer {
                    choice: Choice::Overwrite,
                    apply_to_all: true,
                });
                true
            },
        )
        .await;
        assert_eq!(result.errors.len(), 1);
        assert!(result.moved.is_empty());
        assert!(root.join("a/a/keep.txt").is_file());
    }
}
