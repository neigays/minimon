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
    /// 管理员账号维护
    Admin {
        #[arg(long)]
        config: Option<PathBuf>,
        #[command(subcommand)]
        cmd: AdminCmd,
    },
}

#[derive(Subcommand)]
enum AdminCmd {
    /// 生成新随机密码并打印到 stdout，同时作废所有会话
    ResetPassword,
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

fn main() -> Result<()> {
    tracing_subscriber::fmt::init();
    match Cli::parse().cmd {
        Cmd::Serve { config } => {
            let cfg = ServerConfig::load(config.as_deref())?;
            tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
                .context("创建 tokio 运行时失败")?
                // 在运行时内、任何初始化之前注册退出信号（需要运行时上下文）
                .block_on(async {
                    let shutdown = minimon_server::signal::shutdown_signal();
                    minimon_server::serve(cfg, shutdown).await
                })?;
        }
        Cmd::Admin { config, cmd } => {
            let (_, db) = open_db(config.as_deref())?;
            match cmd {
                AdminCmd::ResetPassword => {
                    let pw = db
                        .reset_admin_password()
                        .context("重置失败（管理员尚未创建时请先启动一次 server）")?;
                    println!("{pw}");
                    eprintln!("新密码已生成，所有会话已作废。请登录后尽快修改。");
                }
            }
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
