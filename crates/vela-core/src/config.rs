//! Per-user server settings and atomic TOML persistence.

use std::{
    collections::HashSet,
    env, fs,
    io::{self, Write},
    os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt, symlink},
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};

use crate::{Auth, ConnectionSpec, askpass::random_hex, edit, settings::Settings};

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct Server {
    pub id: String,
    pub name: String,
    pub host: String,
    pub port: u16,
    pub user: String,
    pub auth: Auth,
    pub initial_remote_path: Option<PathBuf>,
    /// Pinned servers come first. Old files called this `favorite`.
    #[serde(default, alias = "favorite")]
    pub pinned: bool,
}

impl Server {
    pub fn new_id() -> io::Result<String> {
        random_hex::<16>()
    }

    /// The name shown in the sidebar, the session header and dialogs. An empty
    /// name falls back to the host.
    pub fn display_name(&self) -> &str {
        let name = self.name.trim();
        if name.is_empty() { &self.host } else { name }
    }

    pub fn connection_spec(&self) -> ConnectionSpec {
        let mut spec = ConnectionSpec::new(
            self.host.clone(),
            self.port,
            self.user.clone(),
            self.auth.clone(),
        );
        spec.display_name = Some(self.display_name().to_owned());
        spec
    }
}

/// Removes the global agent socket from servers that stored the very same
/// socket, so a single change in Settings applies to them. Returns whether any
/// server changed.
pub fn clear_default_sockets(servers: &mut [Server], default: &Path) -> bool {
    let mut changed = false;
    for server in servers {
        if let Auth::Agent {
            agent_socket: Some(socket),
            ..
        } = &server.auth
            && socket == default
        {
            if let Auth::Agent { agent_socket, .. } = &mut server.auth {
                *agent_socket = None;
            }
            changed = true;
        }
    }
    changed
}

/// The list order is the order of `servers` in the file. Files older than this
/// version were sorted by the application and are sorted once when loaded.
const FILE_VERSION: u32 = 2;

#[derive(Deserialize, Serialize)]
struct Config {
    #[serde(default)]
    version: u32,
    servers: Vec<Server>,
}

pub fn default_directory() -> io::Result<PathBuf> {
    #[cfg(not(target_os = "macos"))]
    if let Some(dir) = env::var_os("XDG_CONFIG_HOME").map(PathBuf::from)
        && dir.is_absolute()
    {
        return Ok(dir.join("vela"));
    }

    let home = env::home_dir()
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "Home directory is unavailable"))?;
    #[cfg(target_os = "macos")]
    let relative = "Library/Application Support/Vela";
    #[cfg(not(target_os = "macos"))]
    let relative = ".config/vela";
    Ok(home.join(relative))
}

/// The configuration directory vSCP used before Vela.
pub fn legacy_directory() -> io::Result<PathBuf> {
    #[cfg(not(target_os = "macos"))]
    if let Some(dir) = env::var_os("XDG_CONFIG_HOME").map(PathBuf::from)
        && dir.is_absolute()
    {
        return Ok(dir.join("vscp"));
    }

    let home = env::home_dir()
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "Home directory is unavailable"))?;
    #[cfg(target_os = "macos")]
    let relative = "Library/Application Support/vSCP";
    #[cfg(not(target_os = "macos"))]
    let relative = ".config/vscp";
    Ok(home.join(relative))
}

pub fn load(dir: &Path) -> io::Result<Vec<Server>> {
    let text = match fs::read_to_string(dir.join("servers.toml")) {
        Ok(text) => text,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(err) => return Err(err),
    };
    let config: Config =
        toml::from_str(&text).map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))?;
    let mut ids = HashSet::new();
    if config
        .servers
        .iter()
        .any(|server| server.id.is_empty() || !ids.insert(&server.id))
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Missing or duplicate server ID",
        ));
    }
    let mut servers = config.servers;
    if config.version < FILE_VERSION {
        servers.sort_by_cached_key(|server| {
            (
                !server.pinned,
                server.name.to_lowercase(),
                server.id.clone(),
            )
        });
    }
    let (mut ordered, unpinned): (Vec<_>, Vec<_>) =
        servers.into_iter().partition(|server| server.pinned);
    ordered.extend(unpinned);
    Ok(ordered)
}

pub fn save(dir: &Path, servers: Vec<Server>) -> io::Result<()> {
    // Refuse to replace settings that cannot be read, including after startup.
    load(dir)?;
    let text = toml::to_string_pretty(&Config {
        version: FILE_VERSION,
        servers,
    })
    .map_err(io::Error::other)?;
    save_private(dir, "servers.toml", &text)
}

/// Pins or unpins `id`. Pinning moves the server to the end of the pinned
/// group and unpinning moves it to the start of the unpinned group. Unknown ids
/// and servers that already have that state are left alone.
pub fn set_pinned(servers: &mut Vec<Server>, id: &str, pinned: bool) {
    let Some(index) = servers.iter().position(|server| server.id == id) else {
        return;
    };
    if servers[index].pinned == pinned {
        return;
    }
    let mut server = servers.remove(index);
    server.pinned = pinned;
    let boundary = servers.iter().filter(|server| server.pinned).count();
    servers.insert(boundary, server);
}

/// Moves `id` into the pinned or unpinned group, before the server `before` if
/// it belongs to that group and at the end of the group otherwise. Dropping a
/// server before itself changes nothing.
pub fn move_server(servers: &mut Vec<Server>, id: &str, pinned: bool, before: Option<&str>) {
    if before == Some(id) {
        return;
    }
    let Some(index) = servers.iter().position(|server| server.id == id) else {
        return;
    };
    let mut server = servers.remove(index);
    server.pinned = pinned;
    let boundary = servers.iter().filter(|server| server.pinned).count();
    let group = if pinned {
        0..boundary
    } else {
        boundary..servers.len()
    };
    let at = before
        .and_then(|before| {
            servers[group.clone()]
                .iter()
                .position(|server| server.id == before)
        })
        .map_or(group.end, |offset| group.start + offset);
    servers.insert(at, server);
}

pub(crate) fn save_private(dir: &Path, name: &str, text: &str) -> io::Result<()> {
    save_private_bytes(dir, name, text.as_bytes())
}

fn save_private_bytes(dir: &Path, name: &str, contents: &[u8]) -> io::Result<()> {
    fs::create_dir_all(dir)?;
    let mut temp = tempfile::NamedTempFile::new_in(dir)?;
    temp.as_file()
        .set_permissions(fs::Permissions::from_mode(0o600))?;
    temp.write_all(contents)?;
    temp.as_file().sync_all()?;
    temp.persist(dir.join(name))?;
    Ok(())
}

/// Migrates the real vSCP configuration into the real Vela directory. Callers
/// using an overridden configuration directory skip this entirely.
pub fn migrate_legacy_directory() -> io::Result<bool> {
    let legacy = legacy_directory()?;
    let target = default_directory()?;
    let legacy_cache = edit::default_root("vSCP")?;
    let target_cache = edit::default_root("Vela")?;
    migrate_with_cache(&legacy, &target, &target_cache, Some(&legacy_cache))
}

/// Imports the legacy configuration and its in-directory edit cache.
pub fn migrate_directory(legacy: &Path, target: &Path) -> io::Result<bool> {
    migrate_with_cache(legacy, target, &target.join("edit-cache"), None)
}

fn migrate_with_cache(
    legacy: &Path,
    target: &Path,
    edit_root: &Path,
    legacy_cache: Option<&Path>,
) -> io::Result<bool> {
    if legacy == target {
        return Ok(false);
    }
    match fs::symlink_metadata(legacy) {
        Ok(meta) if meta.is_dir() => {}
        Ok(_) => return Ok(false),
        Err(err) if err.kind() == io::ErrorKind::NotFound => {
            // The original directory migration copied edits.toml verbatim. Repair
            // outdated paths even if that migration already removed the source config.
            return edit::migrate_index(legacy, target, edit_root, legacy_cache);
        }
        Err(err) => return Err(err),
    }
    let source_dir = legacy.canonicalize()?;
    let target_dir = target.canonicalize().unwrap_or_else(|_| target.to_owned());
    if target.starts_with(legacy) || target_dir.starts_with(&source_dir) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Config directories overlap",
        ));
    }
    // Two new Vela instances must not import and remove the same source at once.
    let migration_lock = fs::File::open(legacy)?;
    migration_lock.try_lock().map_err(io::Error::from)?;

    // Validate source and target before writing anything.
    let previous = load(target)?;
    let imported = load(legacy)?;
    let current = Settings::load(target)?;
    let old_settings = Settings::load(legacy)?;
    let hosts = merge_known_hosts(legacy, target)?;
    let mut servers = {
        let ids: HashSet<_> = previous.iter().map(|server| server.id.as_str()).collect();
        let mut servers = previous.clone();
        servers.extend(
            imported
                .into_iter()
                .filter(|server| !ids.contains(server.id.as_str())),
        );
        servers
    };
    let legacy_keys = legacy.join("keys");
    let target_keys = target.join("keys");
    for server in &mut servers {
        if let Auth::Agent {
            identity_pub,
            agent_socket,
        } = &mut server.auth
        {
            rewrite_path(identity_pub, &legacy_keys, &target_keys);
            if let Some(socket) = agent_socket {
                rewrite_path(socket, legacy, target);
            }
        }
    }
    let mut settings = current.clone();
    settings.import_defaults(old_settings);
    for path in [&mut settings.editor, &mut settings.agent_socket]
        .into_iter()
        .flatten()
    {
        rewrite_path(path, legacy, target);
    }

    private_directory(target)?;
    copy_entries(legacy, target, legacy, target)?;
    if servers != previous
        || (legacy.join("servers.toml").exists() && !target.join("servers.toml").exists())
    {
        save(target, servers)?;
    }
    if settings != current
        || (legacy.join("settings.toml").exists() && !target.join("settings.toml").exists())
    {
        settings.save(target)?;
    }
    if let Some(hosts) = hosts {
        save_private_bytes(target, "known_hosts", &hosts)?;
    }
    edit::migrate_index(legacy, target, edit_root, legacy_cache)?;

    fs::remove_dir_all(legacy)?;
    Ok(true)
}

fn rewrite_path(path: &mut PathBuf, before: &Path, after: &Path) {
    if let Ok(relative) = path.strip_prefix(before) {
        *path = after.join(relative);
    }
}

fn merge_known_hosts(legacy: &Path, target: &Path) -> io::Result<Option<Vec<u8>>> {
    let read = |dir: &Path| match fs::read(dir.join("known_hosts")) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(err),
    };
    let Some(legacy) = read(legacy)? else {
        return Ok(None);
    };
    let mut target = read(target)?.unwrap_or_default();
    let mut seen: HashSet<_> = target
        .split(|byte| *byte == b'\n')
        .map(<[u8]>::trim_ascii)
        .collect();
    let mut additions = Vec::new();
    for line in legacy.split(|byte| *byte == b'\n').map(<[u8]>::trim_ascii) {
        if !line.is_empty() && seen.insert(line) {
            additions.extend_from_slice(line);
            additions.push(b'\n');
        }
    }
    if !additions.is_empty() && !target.is_empty() && !target.ends_with(b"\n") {
        target.push(b'\n');
    }
    target.extend(additions);
    Ok(Some(target))
}

fn private_directory(path: &Path) -> io::Result<()> {
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(path)?;
    if !fs::symlink_metadata(path)?.is_dir() {
        return Err(io::Error::other("Configuration path isn't a directory"));
    }
    Ok(())
}

/// Copies missing files and preserves their modes and modification times.
fn copy_entries(from: &Path, to: &Path, legacy: &Path, target: &Path) -> io::Result<()> {
    for entry in fs::read_dir(from)? {
        let entry = entry?;
        let name = entry.file_name();
        if from == legacy
            && name.to_str().is_some_and(|name| {
                matches!(
                    name,
                    "servers.toml" | "settings.toml" | "known_hosts" | "edits.toml" | "edit-cache"
                )
            })
        {
            continue;
        }
        let source = entry.path();
        let destination = to.join(name);
        let kind = entry.file_type()?;
        if kind.is_dir() {
            let existed = destination.exists();
            private_directory(&destination)?;
            copy_entries(&source, &destination, legacy, target)?;
            if !existed {
                fs::set_permissions(&destination, entry.metadata()?.permissions())?;
            }
        } else {
            match destination.symlink_metadata() {
                Ok(_) => continue,
                Err(err) if err.kind() == io::ErrorKind::NotFound => {}
                Err(err) => return Err(err),
            }
            if kind.is_file() {
                copy_file(&source, &destination)?;
            } else if kind.is_symlink() {
                let mut link = fs::read_link(&source)?;
                rewrite_path(&mut link, legacy, target);
                symlink(link, &destination)?;
            } else {
                return Err(io::Error::other(format!(
                    "Can't migrate {}",
                    source.display()
                )));
            }
        }
    }
    Ok(())
}

pub(crate) fn copy_file(source: &Path, destination: &Path) -> io::Result<()> {
    let mut source = fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(source)?;
    let metadata = source.metadata()?;
    if !metadata.is_file() {
        return Err(io::Error::other("Expected a regular file"));
    }
    let mut temp = tempfile::NamedTempFile::new_in(destination.parent().unwrap())?;
    io::copy(&mut source, &mut temp)?;
    temp.as_file().set_permissions(metadata.permissions())?;
    temp.as_file().set_modified(metadata.modified()?)?;
    temp.as_file().sync_all()?;
    temp.persist_noclobber(destination)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn server(auth: Auth) -> Server {
        Server {
            id: Server::new_id().unwrap(),
            name: "Pruebas".into(),
            host: "test.example".into(),
            port: 2222,
            user: "tester".into(),
            auth,
            initial_remote_path: None,
            pinned: false,
        }
    }

    #[test]
    fn display_name_falls_back_to_the_host() {
        let mut server = server(Auth::Password);
        assert_eq!(server.display_name(), "Pruebas");
        server.name = "   ".into();
        assert_eq!(server.display_name(), "test.example");
        assert_eq!(server.connection_spec().destination(), "test.example");
    }

    #[test]
    fn clearing_the_default_socket_only_touches_matching_servers() {
        let mut servers = vec![
            Server {
                auth: Auth::Agent {
                    identity_pub: "/tmp/a.pub".into(),
                    agent_socket: Some("/tmp/global.sock".into()),
                },
                ..server(Auth::Password)
            },
            Server {
                auth: Auth::Agent {
                    identity_pub: "/tmp/b.pub".into(),
                    agent_socket: Some("/tmp/own.sock".into()),
                },
                ..server(Auth::Password)
            },
            Server {
                auth: Auth::Agent {
                    identity_pub: "/tmp/c.pub".into(),
                    agent_socket: None,
                },
                ..server(Auth::Password)
            },
        ];
        assert!(clear_default_sockets(
            &mut servers,
            Path::new("/tmp/global.sock")
        ));
        assert!(matches!(
            &servers[0].auth,
            Auth::Agent {
                agent_socket: None,
                ..
            }
        ));
        assert_eq!(
            servers[1].auth,
            Auth::Agent {
                identity_pub: "/tmp/b.pub".into(),
                agent_socket: Some("/tmp/own.sock".into()),
            }
        );
        assert!(!clear_default_sockets(
            &mut servers,
            Path::new("/tmp/global.sock")
        ));
    }

    #[test]
    fn round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let servers = vec![
            server(Auth::Password),
            server(Auth::Agent {
                identity_pub: "/tmp/client.pub".into(),
                agent_socket: None,
            }),
        ];
        save(dir.path(), servers.clone()).unwrap();
        assert_eq!(load(dir.path()).unwrap(), servers);
        let metadata = fs::metadata(dir.path().join("servers.toml")).unwrap();
        assert_eq!(metadata.permissions().mode() & 0o777, 0o600);
        assert_eq!(servers[0].connection_spec().destination(), "Pruebas");
    }

    #[test]
    fn missing_config_is_empty() {
        let dir = tempfile::tempdir().unwrap();
        assert!(load(&dir.path().join("missing")).unwrap().is_empty());
    }

    #[test]
    fn corrupt_config_is_preserved() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("servers.toml");
        fs::write(&path, b"[broken").unwrap();
        assert_eq!(
            load(dir.path()).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
        assert_eq!(
            save(dir.path(), Vec::new()).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
        assert_eq!(fs::read(path).unwrap(), b"[broken");
    }

    #[test]
    fn paths_with_spaces_and_enye() {
        let temp = tempfile::tempdir().unwrap();
        let dir = temp.path().join("configuración española");
        let mut server = server(Auth::Agent {
            identity_pub: "/tmp/clave española.pub".into(),
            agent_socket: Some("/tmp/socket español".into()),
        });
        server.initial_remote_path = Some("/home/tester/Archivos españoles".into());
        save(&dir, vec![server.clone()]).unwrap();
        assert_eq!(load(&dir).unwrap(), [server]);
    }

    fn entry(id: &str, name: &str, pinned: bool) -> Server {
        Server {
            id: id.into(),
            name: name.into(),
            pinned,
            ..server(Auth::Password)
        }
    }

    fn ids(servers: &[Server]) -> Vec<&str> {
        servers.iter().map(|server| server.id.as_str()).collect()
    }

    fn write_servers(dir: &Path, header: &str, entries: &[(&str, &str, &str)]) {
        let mut text = header.to_owned();
        for (id, name, flag) in entries {
            text.push_str(&format!(
                "[[servers]]\nid = \"{id}\"\nname = \"{name}\"\nhost = \"h\"\nport = 22\nuser = \"u\"\n{flag}\n[servers.auth]\nkind = \"password\"\n\n"
            ));
        }
        fs::write(dir.join("servers.toml"), text).unwrap();
    }

    #[test]
    fn legacy_favorites_load_in_the_old_order() {
        let dir = tempfile::tempdir().unwrap();
        write_servers(
            dir.path(),
            "",
            &[
                ("3", "alfa", "favorite = false"),
                ("4", "Beta", "favorite = false"),
                ("2", "Zeta", "favorite = true"),
                ("1", "bravo", "favorite = true"),
            ],
        );
        let servers = load(dir.path()).unwrap();
        assert_eq!(ids(&servers), ["1", "2", "3", "4"]);
        assert!(servers[0].pinned && servers[1].pinned && !servers[2].pinned);
        save(dir.path(), servers).unwrap();
        let text = fs::read_to_string(dir.path().join("servers.toml")).unwrap();
        assert!(
            text.contains("version = 2") && text.contains("pinned") && !text.contains("favorite")
        );
    }

    #[test]
    fn version_two_keeps_the_file_order() {
        let dir = tempfile::tempdir().unwrap();
        write_servers(
            dir.path(),
            "version = 2\n",
            &[
                ("a", "zeta", "pinned = true"),
                ("b", "alfa", "pinned = true"),
                ("c", "mu", "pinned = false"),
                ("d", "beta", "pinned = false"),
            ],
        );
        assert_eq!(ids(&load(dir.path()).unwrap()), ["a", "b", "c", "d"]);
    }

    #[test]
    fn load_moves_pinned_servers_first_without_resorting() {
        let dir = tempfile::tempdir().unwrap();
        write_servers(
            dir.path(),
            "version = 2\n",
            &[
                ("a", "z", "pinned = false"),
                ("b", "y", "pinned = true"),
                ("c", "x", "pinned = false"),
                ("d", "w", "pinned = true"),
            ],
        );
        assert_eq!(ids(&load(dir.path()).unwrap()), ["b", "d", "a", "c"]);
    }

    #[test]
    fn pinning_goes_to_the_end_of_the_pinned_group() {
        let mut servers = vec![
            entry("a", "a", true),
            entry("b", "b", true),
            entry("c", "c", false),
            entry("d", "d", false),
            entry("e", "e", false),
        ];
        set_pinned(&mut servers, "e", true);
        assert_eq!(ids(&servers), ["a", "b", "e", "c", "d"]);
        assert!(servers[2].pinned);
        set_pinned(&mut servers, "missing", true);
        set_pinned(&mut servers, "a", true);
        assert_eq!(ids(&servers), ["a", "b", "e", "c", "d"]);
    }

    #[test]
    fn unpinning_goes_to_the_start_of_the_unpinned_group() {
        let mut servers = vec![
            entry("a", "a", true),
            entry("b", "b", true),
            entry("c", "c", true),
            entry("d", "d", false),
        ];
        set_pinned(&mut servers, "a", false);
        assert_eq!(ids(&servers), ["b", "c", "a", "d"]);
        assert!(!servers[2].pinned);
    }

    #[test]
    fn move_server_reorders_within_and_across_groups() {
        let start = || {
            vec![
                entry("a", "a", true),
                entry("b", "b", true),
                entry("c", "c", true),
                entry("d", "d", false),
                entry("e", "e", false),
                entry("f", "f", false),
            ]
        };
        let check = |servers: &[Server]| {
            let boundary = servers.iter().filter(|server| server.pinned).count();
            assert!(servers[..boundary].iter().all(|server| server.pinned));
            assert!(servers[boundary..].iter().all(|server| !server.pinned));
        };

        let mut servers = start();
        move_server(&mut servers, "c", true, Some("a"));
        assert_eq!(ids(&servers), ["c", "a", "b", "d", "e", "f"]);
        check(&servers);

        let mut servers = start();
        move_server(&mut servers, "a", true, None);
        assert_eq!(ids(&servers), ["b", "c", "a", "d", "e", "f"]);
        check(&servers);

        let mut servers = start();
        move_server(&mut servers, "f", false, Some("d"));
        assert_eq!(ids(&servers), ["a", "b", "c", "f", "d", "e"]);
        check(&servers);

        let mut servers = start();
        move_server(&mut servers, "d", false, None);
        assert_eq!(ids(&servers), ["a", "b", "c", "e", "f", "d"]);
        check(&servers);

        let mut servers = start();
        move_server(&mut servers, "b", false, Some("e"));
        assert_eq!(ids(&servers), ["a", "c", "d", "b", "e", "f"]);
        assert!(!servers[3].pinned);
        check(&servers);

        let mut servers = start();
        move_server(&mut servers, "e", true, Some("b"));
        assert_eq!(ids(&servers), ["a", "e", "b", "c", "d", "f"]);
        assert!(servers[1].pinned);
        check(&servers);

        let mut servers = start();
        move_server(&mut servers, "e", true, None);
        assert_eq!(ids(&servers), ["a", "b", "c", "e", "d", "f"]);
        check(&servers);

        // A `before` outside the target group appends to the group.
        let mut servers = start();
        move_server(&mut servers, "a", false, Some("c"));
        assert_eq!(ids(&servers), ["b", "c", "d", "e", "f", "a"]);
        check(&servers);

        let mut servers = start();
        move_server(&mut servers, "b", true, Some("b"));
        move_server(&mut servers, "missing", true, None);
        assert_eq!(ids(&servers), ["a", "b", "c", "d", "e", "f"]);
    }

    #[test]
    fn order_and_pins_survive_a_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let mut servers = vec![
            entry("b", "alfa", true),
            entry("a", "zeta", false),
            entry("c", "mu", false),
        ];
        move_server(&mut servers, "c", true, Some("b"));
        save(dir.path(), servers.clone()).unwrap();
        let loaded = load(dir.path()).unwrap();
        assert_eq!(ids(&loaded), ["c", "b", "a"]);
        assert_eq!(loaded, servers);
    }

    #[test]
    fn fresh_migration_copies_rewrites_and_removes_legacy() {
        let root = tempfile::tempdir().unwrap();
        let legacy = root.path().join("vscp");
        let target = root.path().join("vela");

        fs::create_dir_all(legacy.join("keys")).unwrap();
        fs::set_permissions(legacy.join("keys"), fs::Permissions::from_mode(0o700)).unwrap();
        let key = legacy.join("keys").join("identity.pub");
        fs::write(&key, "ssh-ed25519 AAAA").unwrap();
        fs::set_permissions(&key, fs::Permissions::from_mode(0o600)).unwrap();

        fs::create_dir_all(legacy.join("themes")).unwrap();
        fs::set_permissions(legacy.join("themes"), fs::Permissions::from_mode(0o700)).unwrap();
        fs::write(legacy.join("themes/aurora.toml"), "name = 'Aurora'").unwrap();
        fs::set_permissions(
            legacy.join("themes/aurora.toml"),
            fs::Permissions::from_mode(0o600),
        )
        .unwrap();
        fs::write(legacy.join("settings.toml"), format!(
            "editor = '/usr/bin/nano'\nagent_socket = '{}'\n[appearance]\ntheme = 'vscp'\nlight_theme = 'aurora'\n",
            legacy.join("agent.sock").display(),
        )).unwrap();
        fs::write(legacy.join("known_hosts"), "host key").unwrap();

        let servers = vec![
            Server {
                id: "internal".into(),
                name: "Interna".into(),
                auth: Auth::Agent {
                    identity_pub: key.clone(),
                    agent_socket: None,
                },
                ..server(Auth::Password)
            },
            Server {
                id: "external".into(),
                name: "Externa".into(),
                auth: Auth::Agent {
                    identity_pub: "/tmp/external.pub".into(),
                    agent_socket: None,
                },
                ..server(Auth::Password)
            },
            entry("password", "Password", false),
        ];
        save(&legacy, servers).unwrap();

        assert!(migrate_directory(&legacy, &target).unwrap());
        assert!(!legacy.exists());

        let settings = Settings::load(&target).unwrap();
        assert_eq!(settings.editor, Some("/usr/bin/nano".into()));
        assert_eq!(settings.agent_socket, Some(target.join("agent.sock")));
        assert_eq!(settings.appearance.light_theme, "aurora");
        assert_eq!(settings.appearance.dark_theme, "vela");
        assert!(
            !fs::read_to_string(target.join("settings.toml"))
                .unwrap()
                .contains("vscp")
        );
        assert_eq!(
            fs::read_to_string(target.join("known_hosts")).unwrap(),
            "host key\n"
        );
        let (themes, errors) = crate::appearance::load_user_themes(&target);
        assert!(errors.is_empty(), "{errors:?}");
        assert_eq!(themes[0].id, "aurora");
        assert_eq!(
            fs::read(target.join("keys/identity.pub")).unwrap(),
            b"ssh-ed25519 AAAA"
        );
        let mode = |path: PathBuf| fs::metadata(path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(target.join("keys")), 0o700);
        assert_eq!(mode(target.join("keys/identity.pub")), 0o600);
        assert_eq!(mode(target.join("themes")), 0o700);
        assert_eq!(mode(target.join("themes/aurora.toml")), 0o600);
        assert_eq!(mode(target.join("known_hosts")), 0o600);

        let migrated = load(&target).unwrap();
        assert_eq!(ids(&migrated), ["internal", "external", "password"]);
        assert_eq!(
            migrated[0].auth,
            Auth::Agent {
                identity_pub: target.join("keys/identity.pub"),
                agent_socket: None,
            }
        );
        assert_eq!(
            migrated[1].auth,
            Auth::Agent {
                identity_pub: "/tmp/external.pub".into(),
                agent_socket: None,
            }
        );
    }

    #[test]
    fn migration_is_a_noop_without_a_legacy_directory() {
        let root = tempfile::tempdir().unwrap();
        let legacy = root.path().join("vscp");
        let target = root.path().join("vela");
        assert!(!migrate_directory(&legacy, &target).unwrap());
        assert!(!target.exists());

        fs::create_dir_all(&legacy).unwrap();
        assert!(!migrate_directory(&legacy, &legacy).unwrap());
    }

    #[test]
    fn merge_migration_keeps_target_files_and_appends_missing_servers() {
        let root = tempfile::tempdir().unwrap();
        let legacy = root.path().join("vscp");
        let target = root.path().join("vela");

        fs::create_dir_all(legacy.join("keys")).unwrap();
        fs::write(legacy.join("keys/legacy.pub"), "legacy key").unwrap();
        fs::create_dir_all(legacy.join("themes")).unwrap();
        fs::write(legacy.join("themes/aurora.toml"), "name = 'Legacy Aurora'").unwrap();
        fs::write(
            legacy.join("known_hosts"),
            "shared key\n\nold key\nold key\n",
        )
        .unwrap();
        fs::write(
            legacy.join("settings.toml"),
            "\
editor = '/usr/bin/nano'
agent_socket = '/opt/agent.sock'
terminal_font_size = 15
terminal_copy_on_select = true
terminal_colors = 'kitty'
[appearance]
scheme = 'dark'
light_theme = 'aurora'
dark_theme = 'oceano'
[appearance.glass]
enabled = true
chrome = 65
content = 84
",
        )
        .unwrap();
        save(
            &legacy,
            vec![
                Server {
                    id: "old".into(),
                    name: "De vSCP".into(),
                    auth: Auth::Agent {
                        identity_pub: legacy.join("keys/legacy.pub"),
                        agent_socket: None,
                    },
                    ..server(Auth::Password)
                },
                entry("shared", "Legacy shared", false),
            ],
        )
        .unwrap();

        fs::create_dir_all(target.join("themes")).unwrap();
        fs::write(target.join("themes/aurora.toml"), "name = 'Vela Aurora'").unwrap();
        fs::write(target.join("known_hosts"), "custom key\nshared key").unwrap();
        fs::write(
            target.join("settings.toml"),
            "\
editor = '/usr/bin/vim'
terminal_copy_on_select = false
[appearance]
light_theme = 'lavanda'
[appearance.glass]
chrome = 60
",
        )
        .unwrap();
        save(
            &target,
            vec![
                entry("new", "De Vela", false),
                entry("shared", "Vela shared", false),
            ],
        )
        .unwrap();

        assert!(migrate_directory(&legacy, &target).unwrap());
        assert!(!legacy.exists());

        let settings = Settings::load(&target).unwrap();
        assert_eq!(settings.editor, Some("/usr/bin/vim".into()));
        assert_eq!(settings.agent_socket, Some("/opt/agent.sock".into()));
        assert_eq!(settings.terminal_font_size, Some(15.));
        assert_eq!(settings.terminal_copy_on_select, Some(false));
        assert_eq!(
            settings.terminal_colors,
            crate::settings::TerminalColorSource::Kitty
        );
        assert_eq!(settings.appearance.scheme, crate::appearance::Scheme::Dark);
        assert_eq!(settings.appearance.light_theme, "lavanda");
        assert_eq!(settings.appearance.dark_theme, "oceano");
        assert!(settings.appearance.glass.enabled);
        assert_eq!(settings.appearance.glass.chrome, 60);
        assert_eq!(settings.appearance.glass.content, 84);
        assert_eq!(
            fs::read_to_string(target.join("known_hosts")).unwrap(),
            "custom key\nshared key\nold key\n"
        );
        assert_eq!(
            fs::read(target.join("keys/legacy.pub")).unwrap(),
            b"legacy key"
        );
        assert_eq!(
            fs::read_to_string(target.join("themes/aurora.toml")).unwrap(),
            "name = 'Vela Aurora'"
        );

        let migrated = load(&target).unwrap();
        assert_eq!(migrated.len(), 3);
        assert!(
            migrated
                .iter()
                .any(|s| s.id == "new" && s.name == "De Vela")
        );
        assert!(
            migrated
                .iter()
                .any(|s| s.id == "shared" && s.name == "Vela shared")
        );
        let old = migrated.iter().find(|server| server.id == "old").unwrap();
        assert_eq!(old.name, "De vSCP");
        assert_eq!(
            old.auth,
            Auth::Agent {
                identity_pub: target.join("keys/legacy.pub"),
                agent_socket: None,
            }
        );
    }

    #[test]
    fn corrupt_legacy_servers_abort_without_deleting_legacy() {
        let root = tempfile::tempdir().unwrap();
        let legacy = root.path().join("vscp");
        let target = root.path().join("vela");
        fs::create_dir_all(&legacy).unwrap();
        fs::write(legacy.join("servers.toml"), "[broken").unwrap();
        Settings::default().save(&legacy).unwrap();

        let error = migrate_directory(&legacy, &target).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert!(legacy.exists());
        assert_eq!(fs::read(legacy.join("servers.toml")).unwrap(), b"[broken");
        assert!(!target.exists());
    }

    #[test]
    fn corrupt_legacy_settings_leave_both_directories_intact() {
        let root = tempfile::tempdir().unwrap();
        let legacy = root.path().join("vscp");
        let target = root.path().join("vela");
        save(&legacy, vec![entry("old", "Legacy", false)]).unwrap();
        save(&target, vec![entry("new", "Vela", false)]).unwrap();
        Settings::default().save(&target).unwrap();
        fs::write(legacy.join("settings.toml"), "[appearance\n").unwrap();
        fs::write(legacy.join("known_hosts"), "old key\n").unwrap();

        assert_eq!(
            migrate_directory(&legacy, &target).unwrap_err().kind(),
            io::ErrorKind::InvalidData,
        );
        assert_eq!(load(&target).unwrap()[0].id, "new");
        assert!(!target.join("known_hosts").exists());
        assert_eq!(load(&legacy).unwrap()[0].id, "old");
        assert_eq!(
            fs::read(legacy.join("settings.toml")).unwrap(),
            b"[appearance\n"
        );
    }

    #[test]
    fn import_into_a_saved_default_configuration() {
        let root = tempfile::tempdir().unwrap();
        let legacy = root.path().join("vscp");
        let target = root.path().join("vela");
        fs::create_dir(&legacy).unwrap();
        write_servers(
            &legacy,
            "version = 0\n",
            &[("old", "Legacy", "favorite = true")],
        );
        let old_servers = load(&legacy).unwrap();
        assert!(old_servers[0].pinned);
        let old_settings = Settings {
            editor: Some("/usr/bin/nano".into()),
            terminal_copy_on_select: Some(true),
            ..Settings::default()
        };
        old_settings.save(&legacy).unwrap();
        Settings::default().save(&target).unwrap();
        save(&target, vec![]).unwrap();

        assert!(migrate_directory(&legacy, &target).unwrap());
        assert_eq!(Settings::load(&target).unwrap(), old_settings);
        assert_eq!(load(&target).unwrap(), old_servers);
    }
}
