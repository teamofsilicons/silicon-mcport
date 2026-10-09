//! Bounded protocol handshakes report MCP availability independently of host heartbeats.
//! Probes never invoke tools, read resources, or reuse a different account's credentials.
use super::*;

type Key = (String, String);
struct Entry {
    endpoint: Endpoint,
    fingerprint: [u8; 32],
    generation: Arc<()>,
    checked_at: i64,
    online: bool,
    probing: bool,
    using_existing: bool,
    cancellation: CancellationToken,
}
#[derive(Default)]
pub(super) struct Monitor {
    entries: BTreeMap<Key, Entry>,
    tasks: tokio::task::JoinSet<(Key, Arc<()>, bool)>,
}
impl Monitor {
    pub fn sync(&mut self, registry: &Registry) {
        let mut desired = BTreeMap::new();
        for (id, connection) in &registry.connections {
            let principals = if connection.auth_mode == "per-user" {
                connection.personal_accounts.keys().cloned().collect()
            } else {
                vec![String::new()]
            };
            for principal in principals {
                let actor = Actor {
                    principal_id: principal.clone(),
                    org_id: registry.host.org_id.clone(),
                    ..Default::default()
                };
                if let Ok(endpoint) = endpoint_for(connection, &actor) {
                    desired.insert((id.clone(), principal), endpoint);
                }
            }
        }
        self.entries.retain(|key, entry| {
            let keep = desired
                .get(key)
                .is_some_and(|endpoint| fingerprint(endpoint) == entry.fingerprint);
            if !keep {
                entry.cancellation.cancel();
            }
            keep
        });
        for (key, endpoint) in desired {
            self.entries.entry(key).or_insert_with(|| Entry {
                fingerprint: fingerprint(&endpoint),
                generation: Arc::new(()),
                endpoint,
                checked_at: 0,
                online: false,
                probing: false,
                using_existing: false,
                cancellation: CancellationToken::new(),
            });
        }
    }
    pub fn tick(&mut self, sessions: &SessionPool) {
        while let Some(result) = self.tasks.try_join_next() {
            match result {
                Ok((key, generation, online)) => {
                    if let Some(entry) = self.entries.get_mut(&key)
                        && Arc::ptr_eq(&entry.generation, &generation)
                    {
                        entry.probing = false;
                        entry.online = online;
                        entry.checked_at = now();
                    }
                }
                Err(_) => {
                    // Recover from a failed probe task without leaving entries stuck.
                    self.tasks = tokio::task::JoinSet::new();
                    for entry in self.entries.values_mut() {
                        entry.probing = false;
                    }
                    return;
                }
            }
        }
        for ((connection, principal), entry) in &mut self.entries {
            entry.using_existing = false;
            if !matches!(entry.endpoint, Endpoint::Stdio { .. }) {
                continue;
            }
            // A stdio provider may allow only one live process. Inspect its
            // existing protocol session instead of starting a competing probe.
            for ((session_connection, _, session_principal), session) in sessions {
                if session_connection != connection
                    || (!principal.is_empty() && session_principal != principal)
                {
                    continue;
                }
                match session.try_lock() {
                    Ok(session)
                        if session.fingerprint == entry.fingerprint
                            && session.session.as_ref().is_some_and(|s| !s.is_closed()) =>
                    {
                        entry.checked_at = now();
                        entry.online = true;
                        entry.using_existing = true;
                    }
                    Err(_) => entry.using_existing = true,
                    _ => {}
                }
            }
        }
        while self.tasks.len() < 4 {
            let Some((key, entry)) = self
                .entries
                .iter_mut()
                .filter(|(_, e)| !e.probing && !e.using_existing && e.checked_at <= now() - 30)
                .min_by_key(|(_, e)| e.checked_at)
            else {
                break;
            };
            entry.probing = true;
            let key = key.clone();
            let generation = entry.generation.clone();
            let endpoint = entry.endpoint.clone();
            let cancellation = entry.cancellation.clone();
            self.tasks.spawn(async move {
                let online = probe(&endpoint, cancellation).await;
                (key, generation, online)
            });
        }
    }
    pub fn snapshot(&self) -> BTreeMap<String, Value> {
        let mut connections: BTreeMap<String, Value> = BTreeMap::new();
        for ((connection, principal), entry) in &self.entries {
            let value = connections.entry(connection.clone()).or_insert(json!({}));
            // Age avoids relying on synchronized clocks between host and gateway.
            value[principal] = json!({"age_seconds":(entry.checked_at != 0).then(|| now().saturating_sub(entry.checked_at)),"online":entry.online});
        }
        connections
    }
}
impl Drop for Monitor {
    fn drop(&mut self) {
        for entry in self.entries.values() {
            entry.cancellation.cancel();
        }
    }
}
async fn probe(endpoint: &Endpoint, cancellation: CancellationToken) -> bool {
    let options = ExecutionOptions {
        network_policy: NetworkPolicy::LocalHost,
        connect_timeout: Duration::from_secs(3),
        timeout: Duration::from_secs(3),
        cancellation,
        ..Default::default()
    };
    tokio::time::timeout(Duration::from_secs(6), async {
        match McpSession::connect(endpoint, &options).await {
            Ok(mut session) => {
                let online = !session.is_closed();
                session.close().await;
                online
            }
            Err(_) => false,
        }
    })
    .await
    .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn an_open_http_port_without_an_mcp_is_offline() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(listener, axum::Router::new()).await.unwrap();
        });
        assert!(
            !probe(
                &Endpoint::http(format!("http://{address}/mcp")),
                CancellationToken::new()
            )
            .await
        );
        server.abort();
    }

    #[tokio::test]
    async fn missing_stdio_and_cancelled_probes_are_offline() {
        let endpoint = Endpoint::Stdio {
            command: "/mcport-nonexistent-test-program".into(),
            args: vec![],
            env: BTreeMap::new(),
            cwd: None,
        };
        assert!(!probe(&endpoint, CancellationToken::new()).await);
        let cancellation = CancellationToken::new();
        cancellation.cancel();
        assert!(!probe(&Endpoint::http("http://127.0.0.1:1/mcp"), cancellation).await);
    }

    #[test]
    fn personal_health_is_account_specific_and_revocation_cancels_probe() {
        let mut registry = Registry::new(super::super::tests::host());
        registry
            .register("one", Endpoint::http("http://127.0.0.1:1/mcp"), "per-user")
            .unwrap();
        registry
            .connections
            .get_mut("one")
            .unwrap()
            .personal_accounts
            .insert(
                "alice".into(),
                LocalAccount {
                    bearer_token: Some("alice-secret".into()),
                    ..Default::default()
                },
            );
        let mut monitor = Monitor::default();
        monitor.sync(&registry);
        assert_eq!(monitor.entries.len(), 1);
        let key = ("one".into(), "alice".into());
        let cancellation = monitor.entries[&key].cancellation.clone();
        let report = monitor.snapshot();
        assert!(report["one"].get("alice").is_some());
        assert!(report["one"].get("").is_none());
        assert!(!serde_json::to_string(&report).unwrap().contains("secret"));
        registry.connections.clear();
        monitor.sync(&registry);
        assert!(cancellation.is_cancelled());
        assert!(monitor.snapshot().is_empty());
    }

    #[tokio::test]
    async fn removed_account_probe_cannot_overwrite_an_identical_new_registration() {
        let mut registry = Registry::new(super::super::tests::host());
        registry
            .register("one", Endpoint::http("http://127.0.0.1:1/mcp"), "none")
            .unwrap();
        let mut monitor = Monitor::default();
        monitor.sync(&registry);
        let key = ("one".into(), String::new());
        let old_generation = monitor.entries[&key].generation.clone();
        let connection = registry.connections.remove("one").unwrap();
        monitor.sync(&registry);
        registry.connections.insert("one".into(), connection);
        monitor.sync(&registry);
        let entry = monitor.entries.get_mut(&key).unwrap();
        entry.checked_at = now();
        entry.online = true;
        entry.probing = true;
        monitor
            .tasks
            .spawn(async move { (key, old_generation, false) });
        tokio::task::yield_now().await;
        monitor.tick(&SessionPool::new());
        let entry = &monitor.entries[&("one".into(), String::new())];
        assert!(entry.online);
        assert!(entry.probing);
    }
}
