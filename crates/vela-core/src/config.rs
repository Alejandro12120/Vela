//! Per-user server settings and atomic TOML persistence.

use std::{
    collections::HashSet,
    env, fs,
    io::{self, Write},
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};

use crate::{Auth, ConnectionSpec, askpass::random_hex};

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
/// socket, so a single change in Ajustes applies to them. Returns whether any
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
    fs::create_dir_all(dir)?;
    let mut temp = tempfile::NamedTempFile::new_in(dir)?;
    temp.as_file()
        .set_permissions(fs::Permissions::from_mode(0o600))?;
    temp.write_all(text.as_bytes())?;
    temp.as_file().sync_all()?;
    temp.persist(dir.join(name))?;
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
}
