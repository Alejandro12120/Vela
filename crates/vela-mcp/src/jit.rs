//! Elevaciones temporales y concesiones efímeras, solo en memoria.

use std::{collections::HashMap, time::SystemTime};

/// Concesiones JIT de una sesión. Nunca se escriben en disco y el tiempo se
/// pasa explícitamente a cada método para poder probarlas sin esperas.
#[derive(Clone, Debug, Default)]
pub struct JitGrants {
    profiles: HashMap<String, ProfileGrant>,
    scoped: Vec<ScopedGrant>,
}

#[derive(Clone, Debug)]
struct ProfileGrant {
    profile_id: String,
    expires_at: SystemTime,
}

#[derive(Clone, Debug)]
struct ScopedGrant {
    server_id: String,
    action: String,
    scope: String,
    expires_at: SystemTime,
}

impl JitGrants {
    pub fn new() -> Self {
        Self::default()
    }

    /// Eleva temporalmente el perfil de un servidor.
    pub fn grant_profile(
        &mut self,
        server_id: impl Into<String>,
        profile_id: impl Into<String>,
        expires_at: SystemTime,
    ) {
        self.profiles.insert(
            server_id.into(),
            ProfileGrant {
                profile_id: profile_id.into(),
                expires_at,
            },
        );
    }

    /// Concede una acción concreta sobre un ámbito (por ejemplo una ruta) hasta `expires_at`.
    pub fn grant_scoped_allow(
        &mut self,
        server_id: impl Into<String>,
        action: impl Into<String>,
        scope: impl Into<String>,
        expires_at: SystemTime,
    ) {
        self.scoped.push(ScopedGrant {
            server_id: server_id.into(),
            action: action.into(),
            scope: scope.into(),
            expires_at,
        });
    }

    /// Elimina todas las concesiones de un servidor.
    pub fn revoke_server(&mut self, server_id: &str) {
        self.profiles.remove(server_id);
        self.scoped.retain(|grant| grant.server_id != server_id);
    }

    /// Elimina todas las concesiones, incluidas las de todos los servidores.
    pub fn revoke_all(&mut self) {
        self.profiles.clear();
        self.scoped.clear();
    }

    /// Descarta las concesiones cuyo vencimiento ya se alcanzó.
    pub fn prune_expired(&mut self, now: SystemTime) {
        self.profiles.retain(|_, grant| grant.expires_at > now);
        self.scoped.retain(|grant| grant.expires_at > now);
    }

    /// Perfil efectivo de un servidor en el instante indicado.
    pub fn active_profile_override(&self, server_id: &str, now: SystemTime) -> Option<&str> {
        self.profiles
            .get(server_id)
            .filter(|grant| grant.expires_at > now)
            .map(|grant| grant.profile_id.as_str())
    }

    /// Comprueba una concesión efímera para una acción y un ámbito exactos.
    pub fn has_scoped_allow(
        &self,
        server_id: &str,
        action: &str,
        scope: &str,
        now: SystemTime,
    ) -> bool {
        self.scoped.iter().any(|grant| {
            grant.server_id == server_id
                && grant.action == action
                && grant.scope == scope
                && grant.expires_at > now
        })
    }

    /// Comprueba las concesiones vivas aplicando un predicado al ámbito guardado.
    /// Permite al motor normalizar rutas antes de compararlas con el ámbito.
    pub fn any_scoped_allow(
        &self,
        server_id: &str,
        action: &str,
        now: SystemTime,
        mut matches_scope: impl FnMut(&str) -> bool,
    ) -> bool {
        self.scoped.iter().any(|grant| {
            grant.server_id == server_id
                && grant.action == action
                && grant.expires_at > now
                && matches_scope(&grant.scope)
        })
    }

    /// Número de concesiones vivas en el instante indicado, para introspección.
    pub fn active_count(&self, now: SystemTime) -> usize {
        self.profiles
            .values()
            .filter(|grant| grant.expires_at > now)
            .count()
            + self
                .scoped
                .iter()
                .filter(|grant| grant.expires_at > now)
                .count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, UNIX_EPOCH};

    fn at(secs: u64) -> SystemTime {
        UNIX_EPOCH + Duration::from_secs(secs)
    }

    #[test]
    fn profile_override_is_active_until_expiry() {
        let mut grants = JitGrants::new();
        grants.grant_profile("prod", "development", at(200));
        assert_eq!(
            grants.active_profile_override("prod", at(100)),
            Some("development")
        );
        assert_eq!(grants.active_profile_override("otro", at(100)), None);
    }

    #[test]
    fn profile_override_expires_at_the_exact_instant() {
        let mut grants = JitGrants::new();
        grants.grant_profile("prod", "development", at(200));
        assert_eq!(
            grants.active_profile_override("prod", at(199)),
            Some("development")
        );
        assert_eq!(grants.active_profile_override("prod", at(200)), None);
        assert_eq!(grants.active_profile_override("prod", at(201)), None);
    }

    #[test]
    fn revoke_server_removes_profile_and_scoped_grants() {
        let mut grants = JitGrants::new();
        grants.grant_profile("prod", "development", at(500));
        grants.grant_profile("dev", "development", at(500));
        grants.grant_scoped_allow("prod", "fs_read", "/srv/api", at(500));
        grants.grant_scoped_allow("dev", "fs_read", "/srv/api", at(500));
        grants.revoke_server("prod");
        assert_eq!(grants.active_profile_override("prod", at(100)), None);
        assert_eq!(
            grants.active_profile_override("dev", at(100)),
            Some("development")
        );
        assert!(!grants.has_scoped_allow("prod", "fs_read", "/srv/api", at(100)));
        assert!(grants.has_scoped_allow("dev", "fs_read", "/srv/api", at(100)));
    }

    #[test]
    fn revoke_all_clears_everything() {
        let mut grants = JitGrants::new();
        grants.grant_profile("a", "development", at(500));
        grants.grant_profile("b", "development", at(500));
        grants.grant_scoped_allow("a", "exec", "ls", at(500));
        grants.revoke_all();
        assert_eq!(grants.active_count(at(100)), 0);
    }

    #[test]
    fn scoped_allow_matches_action_and_scope_with_ttl() {
        let mut grants = JitGrants::new();
        grants.grant_scoped_allow("prod", "fs_read", "/tmp/app.log", at(900));
        assert!(grants.has_scoped_allow("prod", "fs_read", "/tmp/app.log", at(100)));
        assert!(!grants.has_scoped_allow("prod", "fs_write", "/tmp/app.log", at(100)));
        assert!(!grants.has_scoped_allow("prod", "fs_read", "/tmp/other", at(100)));
        assert!(!grants.has_scoped_allow("dev", "fs_read", "/tmp/app.log", at(100)));
        assert!(!grants.has_scoped_allow("prod", "fs_read", "/tmp/app.log", at(900)));
    }

    #[test]
    fn any_scoped_allow_applies_a_predicate_to_the_granted_scope() {
        let mut grants = JitGrants::new();
        grants.grant_scoped_allow("prod", "fs_read", "/srv//api/./app.js", at(500));
        assert!(
            grants.any_scoped_allow("prod", "fs_read", at(100), |scope| scope
                .split('/')
                .filter(|part| !part.is_empty() && *part != ".")
                .collect::<Vec<_>>()
                .join("/")
                == "srv/api/app.js")
        );
        assert!(!grants.any_scoped_allow("prod", "fs_read", at(500), |_| true));
    }

    #[test]
    fn prune_expired_removes_only_expired_grants() {
        let mut grants = JitGrants::new();
        grants.grant_profile("a", "development", at(100));
        grants.grant_profile("b", "diagnostics", at(300));
        grants.grant_scoped_allow("a", "exec", "ls", at(50));
        grants.prune_expired(at(200));
        assert_eq!(grants.active_profile_override("a", at(200)), None);
        assert_eq!(
            grants.active_profile_override("b", at(200)),
            Some("diagnostics")
        );
        assert_eq!(grants.active_count(at(200)), 1);
    }
}
