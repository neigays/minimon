use clap::Parser;
use minimon_agent::config::Cli;

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .init();
    let cfg = Cli::parse().resolve()?;
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let result = rt.block_on(minimon_agent::run(
        cfg,
        minimon_agent::signal::shutdown_signal(),
    ));
    // 阻塞中的 DNS 解析线程可能挂住，限时退出，避免超过 systemd 的 TimeoutStopSec
    rt.shutdown_timeout(std::time::Duration::from_secs(1));
    result
}
