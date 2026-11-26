use std::{
    ops::Range,
    path::{Path, PathBuf},
};

use gpui::{AppContext, Context, Entity, Focusable, Window};

use crate::{components::text_input::TextInput, pane::Pane};

pub struct InlineRename {
    pub path: PathBuf,
    pub input: Entity<TextInput>,
    pub busy: bool,
    pub error: Option<&'static str>,
}

impl InlineRename {
    pub fn new(
        path: PathBuf,
        directory: bool,
        window: &mut Window,
        cx: &mut Context<Pane>,
    ) -> Option<Self> {
        let name = path.file_name()?.to_str()?;
        let input = cx.new(|cx| {
            let mut input = TextInput::new(name, "Name", "Name", false, cx).compact();
            input.select_bytes(base_selection(name, directory));
            input
        });
        input.focus_handle(cx).focus(window, cx);
        Some(Self {
            path,
            input,
            busy: false,
            error: None,
        })
    }
}

pub fn base_selection(name: &str, directory: bool) -> Range<usize> {
    if directory {
        return 0..name.len();
    }
    0..Path::new(name)
        .file_stem()
        .map_or(name.len(), |stem| stem.len())
}

pub fn validate(name: &str) -> bool {
    !name.trim().is_empty() && name != "." && name != ".." && !name.contains(['/', '\0'])
}

pub fn folder_name(number: usize) -> String {
    match number {
        1 => "untitled folder".into(),
        n => format!("untitled folder {n}"),
    }
}

pub fn file_name(number: usize) -> String {
    match number {
        1 => "untitled.txt".into(),
        n => format!("untitled {n}.txt"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selects_basename_using_utf8_offsets() {
        for (name, directory, selected) in [
            ("año.txt", false, "año"),
            (".env", false, ".env"),
            ("dos.tar.gz", false, "dos.tar"),
            ("carpeta.2", true, "carpeta.2"),
        ] {
            assert_eq!(&name[base_selection(name, directory)], selected);
        }
    }

    #[test]
    fn validates_and_numbers_names() {
        assert!(validate("espacio # ñ.txt"));
        for name in ["", "  ", ".", "..", "a/b", "a\0b"] {
            assert!(!validate(name));
        }
        assert_eq!(folder_name(1), "untitled folder");
        assert_eq!(folder_name(2), "untitled folder 2");
    }

    #[test]
    fn numbers_text_files_before_the_extension() {
        assert_eq!(file_name(1), "untitled.txt");
        assert_eq!(file_name(2), "untitled 2.txt");
        assert_eq!(file_name(3), "untitled 3.txt");
    }
}
