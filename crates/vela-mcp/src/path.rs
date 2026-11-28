//! Normalización léxica, globbing y ámbitos de rutas locales y remotas.

use std::path::Path;

use thiserror::Error;

use crate::presets::CRITICAL_DENIED_PATHS;

pub const EMPTY_REMOTE_SCOPE_REASON: &str = "No allowed paths are configured.";
pub const EMPTY_LOCAL_SCOPE_REASON: &str = "No allowed local paths are configured.";

/// Error al normalizar una ruta solicitada.
#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum PathError {
    #[error("The path contains a null byte.")]
    NulByte,
    #[error("The path must be absolute: {0}")]
    NotAbsolute(String),
    #[error("The path goes above the root folder: {0}")]
    Traversal(String),
}

/// Resultado de comprobar una ruta contra las listas de control.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PathAccess {
    Allowed {
        matched_rule: String,
    },
    /// Fuera del allowlist: el motor decide entre preguntar y denegar.
    OutsideScope {
        reason: String,
    },
    Denied {
        matched_rule: String,
    },
}

/// Normaliza una ruta absoluta sin acceder al sistema de archivos.
///
/// Colapsa `//`, `.` y `..`; un `..` por encima de `/` es un error.
pub fn normalize_lexical(path: &str) -> Result<String, PathError> {
    if path.as_bytes().contains(&0) {
        return Err(PathError::NulByte);
    }
    if !path.starts_with('/') {
        return Err(PathError::NotAbsolute(path.to_owned()));
    }
    let mut parts: Vec<&str> = Vec::new();
    for part in path.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                if parts.pop().is_none() {
                    return Err(PathError::Traversal(path.to_owned()));
                }
            }
            other => parts.push(other),
        }
    }
    if parts.is_empty() {
        Ok("/".to_owned())
    } else {
        Ok(format!("/{}", parts.join("/")))
    }
}

fn segments(value: &str) -> Vec<&str> {
    let trimmed = value.trim_start_matches('/');
    if trimmed.is_empty() {
        Vec::new()
    } else {
        trimmed.split('/').filter(|part| !part.is_empty()).collect()
    }
}

/// Compara un segmento con un patrón que admite `*` y `?`.
fn segment_match(pattern: &str, text: &str) -> bool {
    let pattern: Vec<char> = pattern.chars().collect();
    let text: Vec<char> = text.chars().collect();
    let mut pi = 0;
    let mut ti = 0;
    let mut star: Option<usize> = None;
    let mut star_match = 0;
    while ti < text.len() {
        if pi < pattern.len() && (pattern[pi] == '?' || pattern[pi] == text[ti]) {
            pi += 1;
            ti += 1;
        } else if pi < pattern.len() && pattern[pi] == '*' {
            star = Some(pi);
            star_match = ti;
            pi += 1;
        } else if let Some(index) = star {
            pi = index + 1;
            star_match += 1;
            ti = star_match;
        } else {
            return false;
        }
    }
    while pi < pattern.len() && pattern[pi] == '*' {
        pi += 1;
    }
    pi == pattern.len()
}

fn match_segments(pattern: &[&str], path: &[&str]) -> bool {
    if pattern.is_empty() {
        return path.is_empty();
    }
    if pattern[0] == "**" {
        if match_segments(&pattern[1..], path) {
            return true;
        }
        return !path.is_empty() && match_segments(pattern, &path[1..]);
    }
    if path.is_empty() {
        return false;
    }
    segment_match(pattern[0], path[0]) && match_segments(&pattern[1..], &path[1..])
}

/// Valida patrones editados en la interfaz según la sintaxis de ámbitos de Vela.
pub fn valid_scope_pattern(pattern: &str, local: bool) -> bool {
    let has_root = pattern.starts_with('/') || pattern == "**" || pattern.starts_with("**/");
    let local_home = local && (pattern == "~" || pattern.starts_with("~/"));
    (has_root || local_home)
        && !pattern.chars().any(char::is_control)
        && !segments(pattern)
            .iter()
            .any(|part| matches!(*part, "." | ".."))
}

/// Compara una ruta con un patrón glob.
///
/// `**` coincide con cero o más segmentos completos; `*` con cero o más
/// caracteres dentro de un segmento y `?` con exactamente un carácter.
pub fn glob_match(pattern: &str, path: &str) -> bool {
    if pattern.as_bytes().contains(&0) || path.as_bytes().contains(&0) {
        return false;
    }
    let pattern = segments(pattern);
    let path = segments(path);
    match_segments(&pattern, &path)
}

/// Expande `~/` al directorio personal indicado.
pub(crate) fn expand_tilde(value: &str, home: Option<&Path>) -> Option<String> {
    if value == "~" || value.starts_with("~/") {
        let home = home?;
        let rest = value.strip_prefix("~/").unwrap_or("");
        let home = home.to_string_lossy();
        Some(if rest.is_empty() {
            home.into_owned()
        } else {
            format!("{}/{}", home.trim_end_matches('/'), rest)
        })
    } else {
        Some(value.to_owned())
    }
}

fn normalize_input(value: &str, home: Option<&Path>) -> Result<String, PathError> {
    if value.as_bytes().contains(&0) {
        return Err(PathError::NulByte);
    }
    let expanded =
        expand_tilde(value, home).ok_or_else(|| PathError::NotAbsolute(value.to_owned()))?;
    normalize_lexical(&expanded)
}

fn access_for(
    requested: &str,
    canonical: Option<&str>,
    allowed: &[String],
    denied: &[String],
    home: Option<&Path>,
    empty_scope_reason: &str,
) -> PathAccess {
    let requested_norm = match normalize_input(requested, home) {
        Ok(path) => path,
        Err(err) => {
            return PathAccess::Denied {
                matched_rule: format!("invalid_path: {err}"),
            };
        }
    };
    let canonical_norm = match canonical {
        Some(value) => match normalize_input(value, home) {
            Ok(path) => Some(path),
            Err(err) => {
                return PathAccess::Denied {
                    matched_rule: format!("invalid_canonical_path: {err}"),
                };
            }
        },
        None => None,
    };

    // La denylist se evalúa primero y sobre las dos rutas: un enlace simbólico
    // no puede sortearla apuntando a un archivo sensible.
    for pattern in denied {
        let Some(expanded) = expand_tilde(pattern, home) else {
            continue;
        };
        if glob_match(&expanded, &requested_norm)
            || canonical_norm
                .as_ref()
                .is_some_and(|path| glob_match(&expanded, path))
        {
            return PathAccess::Denied {
                matched_rule: pattern.clone(),
            };
        }
    }

    if allowed.is_empty() {
        return PathAccess::OutsideScope {
            reason: empty_scope_reason.to_owned(),
        };
    }

    let mut matched_rule = None;
    for pattern in allowed {
        let Some(expanded) = expand_tilde(pattern, home) else {
            continue;
        };
        if glob_match(&expanded, &requested_norm) {
            matched_rule = Some(pattern.clone());
            break;
        }
    }
    let Some(matched_rule) = matched_rule else {
        return PathAccess::OutsideScope {
            reason: format!("{requested_norm} is outside the allowed paths."),
        };
    };
    if let Some(canonical) = canonical_norm.as_ref() {
        let canonical_allowed = allowed
            .iter()
            .filter_map(|pattern| expand_tilde(pattern, home))
            .any(|expanded| glob_match(&expanded, canonical));
        if !canonical_allowed {
            return PathAccess::OutsideScope {
                reason: format!("{canonical} is outside the allowed paths."),
            };
        }
    }
    PathAccess::Allowed { matched_rule }
}

/// Comprueba una ruta remota. La denylist crítica se aplica siempre, además de
/// la denylist del perfil y del servidor.
pub fn check_remote_path(
    requested: &str,
    canonical: Option<&str>,
    allowed: &[String],
    denied: &[String],
) -> PathAccess {
    let critical = CRITICAL_DENIED_PATHS
        .iter()
        .map(|pattern| (*pattern).to_owned());
    // Las reglas explícitas van primero para que `matched_rule` identifique la
    // del usuario; la crítica se añade después pero se evalúa igualmente.
    let all_denied: Vec<String> = denied.iter().cloned().chain(critical).collect();
    access_for(
        requested,
        canonical,
        allowed,
        &all_denied,
        None,
        EMPTY_REMOTE_SCOPE_REASON,
    )
}

/// Comprueba una ruta local. `~/.ssh/**` y el directorio de configuración de
/// Vela están bloqueados siempre, aunque el usuario permita `~/**`.
pub fn check_local_path(
    requested: &str,
    canonical: Option<&str>,
    home_dir: Option<&Path>,
    vela_config_dir: Option<&Path>,
    allowed: &[String],
) -> PathAccess {
    // Se aplica siempre, también sin directorio personal conocido: rutas como
    // `/home/alice/.ssh/id_rsa` o `/root/.ssh/config` siguen bloqueadas.
    let mut fixed_denied = vec!["**/.ssh/**".to_owned()];
    if let Some(home) = home_dir {
        fixed_denied.push(expand_tilde("~/.ssh/**", Some(home)).unwrap());
    }
    if let Some(config_dir) = vela_config_dir
        && [Some(requested), canonical]
            .into_iter()
            .flatten()
            .any(|path| {
                normalize_input(path, home_dir)
                    .is_ok_and(|path| Path::new(&path).starts_with(config_dir))
            })
    {
        return PathAccess::Denied {
            matched_rule: "vela_config_dir".into(),
        };
    }
    access_for(
        requested,
        canonical,
        allowed,
        &fixed_denied,
        home_dir,
        EMPTY_LOCAL_SCOPE_REASON,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn normalize_collapses_separators_and_dot_segments() {
        assert_eq!(
            normalize_lexical("/srv//api/./app.js").unwrap(),
            "/srv/api/app.js"
        );
        assert_eq!(
            normalize_lexical("/srv/api/../api/x").unwrap(),
            "/srv/api/x"
        );
        assert_eq!(normalize_lexical("/").unwrap(), "/");
        assert_eq!(normalize_lexical("/a/..").unwrap(), "/");
    }

    #[test]
    fn normalize_rejects_relative_paths() {
        assert_eq!(
            normalize_lexical("srv/api"),
            Err(PathError::NotAbsolute("srv/api".to_owned()))
        );
        assert_eq!(
            normalize_lexical("./srv"),
            Err(PathError::NotAbsolute("./srv".to_owned()))
        );
    }

    #[test]
    fn normalize_rejects_traversal_above_root() {
        assert!(matches!(
            normalize_lexical("../../etc/shadow"),
            Err(PathError::NotAbsolute(_))
        ));
        assert!(matches!(
            normalize_lexical("/srv/../../etc/shadow"),
            Err(PathError::Traversal(_))
        ));
    }

    #[test]
    fn normalize_rejects_nul_bytes() {
        assert_eq!(normalize_lexical("/srv/\0api"), Err(PathError::NulByte));
        assert!(!glob_match("**", "/srv/\0api"));
    }

    #[test]
    fn glob_double_star_matches_zero_or_more_segments() {
        assert!(glob_match("**/.env", "/.env"));
        assert!(glob_match("**/.env*", "/etc/nginx/.env"));
        assert!(glob_match("**/.env*", "/srv/api/.env.local"));
        assert!(glob_match("/srv/api/**", "/srv/api"));
        assert!(glob_match("/srv/api/**", "/srv/api/a/b.ts"));
        assert!(!glob_match("/srv/api/**", "/srv/apiv2/a"));
    }

    #[test]
    fn glob_single_star_does_not_cross_segments() {
        assert!(glob_match("/srv/*/app.js", "/srv/api/app.js"));
        assert!(!glob_match("/srv/*/app.js", "/srv/api/sub/app.js"));
        assert!(glob_match("**/*.key", "/etc/ssl/private.key"));
    }

    #[test]
    fn glob_question_mark_matches_exactly_one_character() {
        assert!(glob_match("/srv/api?.js", "/srv/api1.js"));
        assert!(!glob_match("/srv/api?.js", "/srv/api12.js"));
        assert!(!glob_match("/srv/api?.js", "/srv/api/.js"));
    }

    #[test]
    fn remote_path_allowed_and_outside_scope() {
        assert_eq!(
            check_remote_path("/srv/api/app.js", None, &["/srv/api/**".to_owned()], &[]),
            PathAccess::Allowed {
                matched_rule: "/srv/api/**".to_owned()
            }
        );
        assert!(matches!(
            check_remote_path("/etc/passwd", None, &["/srv/api/**".to_owned()], &[]),
            PathAccess::OutsideScope { .. }
        ));
        assert!(matches!(
            check_remote_path("/srv/api/app.js", None, &[], &[]),
            PathAccess::OutsideScope { .. }
        ));
    }

    #[test]
    fn remote_denylist_always_wins() {
        assert_eq!(
            check_remote_path(
                "/srv/api/.env.local",
                None,
                &["/srv/**".to_owned()],
                &["/srv/api/.env*".to_owned()]
            ),
            PathAccess::Denied {
                matched_rule: "/srv/api/.env*".to_owned()
            }
        );
        assert!(matches!(
            check_remote_path("/etc/shadow", None, &["/**".to_owned()], &[]),
            PathAccess::Denied { .. }
        ));
    }

    #[test]
    fn remote_canonical_path_is_checked_too() {
        assert!(matches!(
            check_remote_path(
                "/srv/api/x",
                Some("/etc/shadow"),
                &["/srv/api/**".to_owned()],
                &[]
            ),
            PathAccess::Denied { .. }
        ));
        assert!(matches!(
            check_remote_path(
                "/srv/api/x",
                Some("/var/lib/data"),
                &["/srv/api/**".to_owned()],
                &[]
            ),
            PathAccess::OutsideScope { .. }
        ));
    }

    #[test]
    fn remote_traversal_and_nul_are_denied() {
        assert!(matches!(
            check_remote_path("/srv/../../etc/passwd", None, &["/**".to_owned()], &[]),
            PathAccess::Denied { .. }
        ));
        assert!(matches!(
            check_remote_path("/srv/\0x", None, &["/**".to_owned()], &[]),
            PathAccess::Denied { .. }
        ));
    }

    #[test]
    fn local_tilde_expands_and_scopes() {
        let home = PathBuf::from("/home/alice");
        assert_eq!(
            check_local_path(
                "~/proyectos/app.js",
                None,
                Some(&home),
                None,
                &["~/proyectos/**".to_owned()]
            ),
            PathAccess::Allowed {
                matched_rule: "~/proyectos/**".to_owned()
            }
        );
        assert!(matches!(
            check_local_path(
                "~/otro/app.js",
                None,
                Some(&home),
                None,
                &["~/proyectos/**".to_owned()]
            ),
            PathAccess::OutsideScope { .. }
        ));
    }

    #[test]
    fn local_fixed_denylist_blocks_ssh_even_with_broad_allowlist() {
        let home = PathBuf::from("/home/alice");
        assert!(matches!(
            check_local_path(
                "~/.ssh/id_ed25519",
                None,
                Some(&home),
                None,
                &["~/**".to_owned()]
            ),
            PathAccess::Denied { .. }
        ));
        assert!(matches!(
            check_local_path(
                "/home/alice/.ssh",
                None,
                Some(&home),
                None,
                &["/home/alice/**".to_owned()]
            ),
            PathAccess::Denied { .. }
        ));
    }

    #[test]
    fn local_ssh_paths_are_denied_even_without_a_home_directory() {
        for path in [
            "/home/alice/.ssh/id_rsa",
            "/home/alice/.ssh",
            "/root/.ssh/config",
        ] {
            assert!(
                matches!(
                    check_local_path(path, None, None, None, &["/**".to_owned()]),
                    PathAccess::Denied { .. }
                ),
                "{path}"
            );
        }
    }

    #[test]
    fn local_fixed_denylist_blocks_vela_config_dir() {
        let home = PathBuf::from("/home/alice");
        let config = PathBuf::from("/home/alice/.config/vela");
        assert!(matches!(
            check_local_path(
                "/home/alice/.config/vela/mcp.toml",
                None,
                Some(&home),
                Some(&config),
                &["/**".to_owned()]
            ),
            PathAccess::Denied { .. }
        ));
    }

    #[test]
    fn local_canonical_symlink_escape_is_blocked() {
        let home = PathBuf::from("/home/alice");
        assert!(matches!(
            check_local_path(
                "/home/alice/proyectos/app.js",
                Some("/home/alice/.ssh/id_rsa"),
                Some(&home),
                None,
                &["/home/alice/proyectos/**".to_owned()]
            ),
            PathAccess::Denied { .. }
        ));
    }
}
