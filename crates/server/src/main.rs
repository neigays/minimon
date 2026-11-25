use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use minimon_server::config::ServerConfig;
use minimon_server::db::Db;
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "minimon-server", version)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// 启动服务
    Serve {
        #[arg(long)]
        config: Option<PathBuf>,
    },
    /// 节点管理
    Node {
        #[arg(long)]
        config: Option<PathBuf>,
        #[command(subcommand)]
        cmd: NodeCmd,
    },
}

#[derive(Subcommand)]
enum NodeCmd {
    /// 新增节点并打印 token（仅显示一次）
    Add { name: String },
    /// 列出节点
    List,
}

fn open_db(config: Option<&std::path::Path>) -> Result<(ServerConfig, Db)> {
    let cfg = ServerConfig::load(config)?;
    std::fs::create_dir_all(&cfg.data_dir)
        .with_context(|| format!("创建数据目录 {} 失败", cfg.data_dir.display()))?;
    let db = Db::open(&cfg.db_path()).context("打开数据库失败")?;
    Ok((cfg, db))
}

async fn shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        if let Ok(mut term) = signal(SignalKind::terminate()) {
            tokio::select! {
                _ = tokio::signal::ctrl_c() => {}
                _ = term.recv() => {}
            }
            return;
        }
    }
    let _ = tokio::signal::ctrl_c().await;
}

fn main() -> Result<()> {
    tracing_subscriber::fmt::init();
    match Cli::parse().cmd {
        Cmd::Serve { config } => {
            let cfg = ServerConfig::load(config.as_deref())?;
            tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
                .context("创建 tokio 运行时失败")?
                .block_on(minimon_server::serve(cfg, shutdown_signal()))?;
        }
        Cmd::Node { config, cmd } => {
            let (cfg, db) = open_db(config.as_deref())?;
            match cmd {
                NodeCmd::Add { name } => {
                    let (node, token) = db.add_node(&name)?;
                    println!("节点已创建: id={} name={}", node.id, node.name);
                    println!("token: {token}");
                    println!("token 只显示这一次，请妥善保存。agent 启动示例:");
                    println!(
                        "  minimon-agent --server ws://<host>:{} --token {token}",
                        cfg.listen.port()
                    );
                }
                NodeCmd::List => {
                    for n in db.list_nodes()? {
                        println!(
                            "{}\t{}\tsort={}\t{}",
                            n.id,
                            n.name,
                            n.sort,
                            if n.hidden { "hidden" } else { "visible" }
                        );
                    }
                }
            }
        }
    }
    Ok(())
}
