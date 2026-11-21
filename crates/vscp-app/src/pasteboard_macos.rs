use std::{
    ffi::{CStr, CString, OsStr},
    io,
    os::unix::ffi::OsStrExt,
    path::{Path, PathBuf},
    ptr::NonNull,
};

use objc2::{ClassType, rc::Retained, runtime::ProtocolObject};
use objc2_app_kit::{NSPasteboard, NSPasteboardWriting};
use objc2_foundation::{NSArray, NSURL};

fn file_url(path: &Path, directory: bool) -> io::Result<Retained<NSURL>> {
    let bytes = CString::new(path.as_os_str().as_bytes())?;
    let pointer = NonNull::from(bytes.as_c_str().to_bytes_with_nul()).cast();
    // The URL initializer copies the NUL-terminated representation.
    Ok(unsafe {
        NSURL::fileURLWithFileSystemRepresentation_isDirectory_relativeToURL(
            pointer, directory, None,
        )
    })
}

fn file_path(url: &NSURL) -> PathBuf {
    // The filesystem representation decomposes Unicode on macOS. Prefer the
    // URL path so an upload can keep the original Unicode spelling.
    if let Some(path) = url.path() {
        return PathBuf::from(path.to_string());
    }
    // The representation is attached to the live file URL and is NUL terminated.
    let bytes = unsafe { CStr::from_ptr(url.fileSystemRepresentation().as_ptr()) }.to_bytes();
    PathBuf::from(OsStr::from_bytes(bytes))
}

pub fn paths() -> Vec<PathBuf> {
    read_paths(&NSPasteboard::generalPasteboard())
}

fn read_paths(board: &NSPasteboard) -> Vec<PathBuf> {
    let classes = NSArray::from_slice(&[NSURL::class()]);
    // NSURL is a pasteboard-reading class. Only accept file URLs from the result.
    let objects = unsafe { board.readObjectsForClasses_options(&classes, None) };
    objects
        .iter()
        .flat_map(|array| array.iter())
        .filter_map(|object| {
            let url = object.downcast_ref::<NSURL>()?;
            url.isFileURL().then(|| file_path(url))
        })
        .collect()
}

pub fn write(paths: &[(PathBuf, bool)]) -> io::Result<()> {
    write_paths(&NSPasteboard::generalPasteboard(), paths)
}

fn write_paths(board: &NSPasteboard, paths: &[(PathBuf, bool)]) -> io::Result<()> {
    let urls = paths
        .iter()
        .map(|(path, directory)| file_url(path, *directory))
        .collect::<io::Result<Vec<_>>>()?;
    let writers = urls
        .iter()
        .map(|url| ProtocolObject::<dyn NSPasteboardWriting>::from_ref(&**url))
        .collect::<Vec<_>>();
    let objects = NSArray::from_slice(&writers);
    board.clearContents();
    if !board.writeObjects(&objects) {
        return Err(io::Error::other(
            "No se pudieron copiar los archivos al portapapeles",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_urls_round_trip_through_a_private_pasteboard() {
        let board = NSPasteboard::pasteboardWithUniqueName();
        let root = tempfile::tempdir().unwrap();
        let file = root.path().join("un archivo # ñ.txt");
        let folder = root.path().join("una carpeta # ñ");
        std::fs::write(&file, b"x").unwrap();
        std::fs::create_dir(&folder).unwrap();
        let url = file_url(&file, false).unwrap();
        let encoded = url.absoluteString().unwrap().to_string();
        assert!(encoded.starts_with("file:///"));
        assert!(
            encoded.ends_with("/un%20archivo%20%23%20%C3%B1.txt"),
            "{encoded}"
        );
        write_paths(&board, &[(file.clone(), false), (folder.clone(), true)]).unwrap();
        assert_eq!(read_paths(&board), [file, folder]);
        board.clearContents();
    }
}
