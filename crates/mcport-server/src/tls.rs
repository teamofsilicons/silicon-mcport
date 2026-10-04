/// Reqwest and Space Station link different Rustls providers. Select one before any
/// background clients start: Tungstenite otherwise cannot infer a process default.
pub fn initialize() -> anyhow::Result<()> {
    rustls::crypto::aws_lc_rs::default_provider()
        .install_default()
        .map_err(|_| {
            anyhow::anyhow!("The process TLS provider was initialized before server startup")
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn startup_selects_tls_provider_with_both_dependencies_linked() {
        const CHILD: &str = "MCPORT_TLS_STARTUP_TEST_CHILD";
        if std::env::var_os(CHILD).is_some() {
            assert!(rustls::crypto::CryptoProvider::get_default().is_none());
            let hook = std::panic::take_hook();
            std::panic::set_hook(Box::new(|_| {}));
            let ambiguous = std::panic::catch_unwind(rustls::ClientConfig::builder).is_err();
            std::panic::set_hook(hook);
            assert!(
                ambiguous,
                "regression requires both linked crypto providers"
            );

            initialize().unwrap();
            // This is the default-provider path used by the telemetry WebSocket client.
            let _config = rustls::ClientConfig::builder()
                .with_root_certificates(rustls::RootCertStore::empty())
                .with_no_client_auth();
            return;
        }

        // A separate process gives the test a fresh process-wide provider regardless
        // of other tests or their execution order, and uses the server's dependency set.
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "tls::tests::startup_selects_tls_provider_with_both_dependencies_linked",
                "--nocapture",
            ])
            .env(CHILD, "1")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "TLS startup subprocess failed: {}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
}
