use std::path::Path;

/// A path shortened for a settings row: `$HOME` becomes `~`, and an overlong
/// path keeps its leading components and its file name with `…` in between.
pub fn abbreviate_path(path: &Path, max_chars: usize) -> String {
    let mut text = path.to_string_lossy().into_owned();
    if let Some(home) = std::env::home_dir() {
        let home = home.to_string_lossy();
        if let Some(rest) = text.strip_prefix(home.as_ref())
            && (rest.is_empty() || rest.starts_with('/'))
        {
            text = format!("~{rest}");
        }
    }
    if text.chars().count() <= max_chars {
        return text;
    }
    let parts: Vec<&str> = text.split('/').filter(|part| !part.is_empty()).collect();
    let Some(file) = parts.last() else {
        return text;
    };
    let root = if text.starts_with('/') { "/" } else { "" };
    let head = format!(
        "{root}{}",
        parts.iter().take(3).copied().collect::<Vec<_>>().join("/")
    );
    let shortened = format!("{head}/…/{file}");
    if shortened.chars().count() <= max_chars {
        return shortened;
    }
    let head = format!("{root}{}", parts.first().copied().unwrap_or("~"));
    let ellipsis = "/…/";
    let budget = max_chars.saturating_sub(ellipsis.chars().count());
    let head_budget = budget / 2;
    let tail_budget = budget - head_budget;
    let head: String = head.chars().take(head_budget).collect();
    let tail: String = file.chars().take(tail_budget).collect();
    format!("{head}{ellipsis}{tail}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_paths_are_untouched() {
        let path = Path::new("/tmp/corta");
        assert_eq!(abbreviate_path(path, 52), "/tmp/corta");
    }

    #[test]
    fn home_is_replaced_before_shortening() {
        let home = std::env::home_dir().unwrap();
        let path = home.join("carpeta/archivo.txt");
        assert_eq!(abbreviate_path(&path, 52), "~/carpeta/archivo.txt");
    }

    #[test]
    fn long_paths_keep_the_head_and_the_file_name() {
        let path = Path::new("/var/Library/Containers/grupo/.bitwarden-ssh-agent.sock");
        let result = abbreviate_path(path, 52);
        assert!(result.starts_with("/var/Library/Containers/…"), "{result}");
        assert!(result.ends_with("/.bitwarden-ssh-agent.sock"), "{result}");
        assert!(result.chars().count() <= 52);
    }

    #[test]
    fn non_home_paths_still_fit_the_limit() {
        let path = Path::new("/var/lib/una/carpeta/muy/larga/con/un/archivo.txt");
        let result = abbreviate_path(path, 20);
        assert!(result.contains('…'));
        assert!(result.chars().count() <= 20, "{result}");
    }
}
