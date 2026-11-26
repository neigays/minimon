use clap::Parser;
use minimon_agent::config::Cli;

fn main() -> anyhow::Result<()> {
    // 输出到日志文件（OpenRC / launchd）时不带颜色控制字符，只在终端里着色。
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .with_ansi(std::io::IsTerminal::is_terminal(&std::io::stdout()))
        .init();
    let cfg = Cli::parse().resolve()?;
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    // 在运行时内、任何初始化之前注册退出信号（需要运行时上下文）。
    let result = rt.block_on(async {
        let shutdown = minimon_agent::signal::shutdown_signal();
        minimon_agent::run(cfg, shutdown).await
    });
    // 阻塞中的 DNS 解析线程可能挂住，限时退出，避免超过 systemd 的 TimeoutStopSec
    rt.shutdown_timeout(std::time::Duration::from_secs(1));
    result
}
