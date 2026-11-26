use std::{cell::Cell, collections::HashMap, fs, path::Path, rc::Rc};

use gpui::{Entity, TestAppContext, VisualTestContext};

use super::{Pane, Source, bind_keys};
use crate::{components::text_input, context_menu::Action};

fn open_pane<'a>(
    root: &Path,
    cx: &'a mut TestAppContext,
) -> (Entity<Pane>, &'a mut VisualTestContext) {
    cx.update(|cx| {
        text_input::bind_keys(cx);
        bind_keys(cx);
    });
    let root = root.to_path_buf();
    let (pane, cx) = cx.add_window_view(|window, cx| {
        let mut pane = Pane::new(
            Source::Local,
            root,
            Rc::new(std::cell::RefCell::new(HashMap::new())),
            Rc::new(Cell::new(false)),
            cx.focus_handle(),
            cx.focus_handle(),
            cx,
        );
        pane.start(window, cx);
        pane
    });
    cx.run_until_parked();
    (pane, cx)
}

#[gpui::test]
async fn enter_commits_inline_rename(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("a.txt"), "contenido").unwrap();
    let (pane, cx) = open_pane(dir.path(), cx);

    pane.update_in(cx, |pane, window, cx| pane.dev_state("rename", window, cx));
    cx.run_until_parked();
    cx.simulate_input("b");
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();

    assert!(dir.path().join("b.txt").exists());
    assert!(!dir.path().join("a.txt").exists());
}

#[gpui::test]
async fn enter_commits_new_folder_name(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (pane, cx) = open_pane(dir.path(), cx);

    let root = dir.path().to_path_buf();
    pane.update_in(cx, |pane, window, cx| pane.new_folder(root, window, cx));
    cx.run_until_parked();
    cx.simulate_input("nueva");
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();

    let names: Vec<_> = fs::read_dir(dir.path())
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    assert_eq!(names, ["nueva"]);
}

#[gpui::test]
async fn new_text_file_never_overwrites_a_local_file(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let original = dir.path().join("untitled.txt");
    fs::write(&original, b"keep this").unwrap();
    let path = Source::Local
        .create_file(dir.path().to_owned(), &cx.executor())
        .await
        .unwrap();

    assert_eq!(path, dir.path().join("untitled 2.txt"));
    assert_eq!(fs::read(path).unwrap(), b"");
    assert_eq!(fs::read(original).unwrap(), b"keep this");
}

#[gpui::test]
async fn enter_commits_new_text_file_with_an_edited_extension(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (pane, cx) = open_pane(dir.path(), cx);

    pane.update_in(cx, |pane, window, cx| {
        pane.menu_action(Action::NewFile, window, cx)
    });
    cx.run_until_parked();
    let file = dir.path().join("untitled.txt");
    assert!(file.is_file());

    cx.simulate_input("x");
    pane.read_with(cx, |pane, cx| {
        assert_eq!(
            pane.rename.as_ref().unwrap().input.read(cx).value(),
            "x.txt"
        );
    });
    let select_all = if cfg!(target_os = "macos") {
        "cmd-a"
    } else {
        "ctrl-a"
    };
    cx.simulate_keystrokes(select_all);
    cx.simulate_input("x.py");
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();

    assert_eq!(fs::read(dir.path().join("x.py")).unwrap(), b"");
    assert!(!file.exists());
}
