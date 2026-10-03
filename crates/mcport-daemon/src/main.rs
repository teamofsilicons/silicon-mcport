#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() || args.iter().any(|arg| arg == "--help" || arg == "-h") {
        println!(
            "Silicon MCPort local host connector\n\nUsage: mcport-daemon --registry <absolute-path>\n\nThe registry is created by mcport host new and contains locally approved endpoints.\nThe daemon connects outward; it does not expose a public port."
        );
        return;
    }
    if args.len() != 2 || args[0] != "--registry" || !std::path::Path::new(&args[1]).is_absolute() {
        eprintln!("Use mcport-daemon --registry <absolute-path>. Run --help for details.");
        std::process::exit(2);
    }
    let shutdown = tokio_util::sync::CancellationToken::new();
    let signal = shutdown.clone();
    tokio::spawn(async move {
        let _ = tokio::signal::ctrl_c().await;
        signal.cancel();
    });
    if let Err(error) = mcport_daemon::run(&args[1], shutdown).await {
        eprintln!("{error}");
        std::process::exit(1);
    }
}
