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
    rt.block_on(minimon_agent::run(cfg, async {
        if let Err(e) = tokio::signal::ctrl_c().await {
            tracing::warn!("监听 Ctrl-C 失败: {e}");
            std::future::pending::<()>().await;
        }
    }))
}
