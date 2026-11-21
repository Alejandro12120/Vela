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
    #[serde(default)]
    pub favorite: bool,
}

impl Server {
    pub fn new_id() -> io::Result<String> {
        random_hex::<16>()
    }

    pub fn connection_spec(&self) -> ConnectionSpec {
        let mut spec = ConnectionSpec::new(
            self.host.clone(),
            self.port,
            self.user.clone(),
            self.auth.clone(),
        );
        spec.display_name = Some(self.name.clone());
        spec
    }
}

#[derive(Deserialize, Serialize)]
struct Config {
    servers: Vec<Server>,
}

pub fn default_directory() -> io::Result<PathBuf> {
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
    Ok(config.servers)
}

pub fn save(dir: &Path, servers: Vec<Server>) -> io::Result<()> {
    // Refuse to replace settings that cannot be read, including after startup.
    load(dir)?;
    let text = toml::to_string_pretty(&Config { servers }).map_err(io::Error::other)?;
    save_private(dir, "servers.toml", &text)
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
            favorite: true,
        }
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
}
