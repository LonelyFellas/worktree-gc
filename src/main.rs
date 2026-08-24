//! wtgc CLI。
//!
//! 默认行为是**只报告，不动任何东西**。这不是保守，是这个工具的契约：
//! 定时任务会无人值守地跑它，一个默认就删东西的工具没资格进 crontab。

use clap::Parser;
use std::io::Write;
use std::path::PathBuf;
use std::time::Duration;
use wtgc::config::ScanConfig;
use wtgc::gates::SystemClock;
use wtgc::scan::{Env, scan};
use wtgc::{discover, forge, git, platform, report};

#[derive(Parser)]
#[command(
    name = "wtgc",
    about = "安全回收 AI coding agent 留下的 git worktree 所占磁盘",
    version
)]
struct Cli {
    /// 要扫描的仓库路径，可重复。不指定则只扫描种子目录。
    #[arg(long, value_name = "PATH")]
    repo: Vec<PathBuf>,

    /// 额外的种子目录，可重复。默认已含已知的 agent worktree 落点。
    #[arg(long, value_name = "PATH")]
    seed: Vec<PathBuf>,

    /// 输出 JSON。供 agent 与脚本消费。
    #[arg(long)]
    json: bool,

    /// 不查询 forge（GitHub 等）。squash-merge 的判定会降级到离线 diff。
    #[arg(long)]
    offline: bool,

    /// worktree 空闲多少小时才考虑处置。
    #[arg(long, value_name = "N", default_value_t = 24)]
    idle_hours: u64,

    /// 缓存目录安静多少分钟才认为构建已结束。
    #[arg(long, value_name = "N", default_value_t = 10)]
    cache_quiet_mins: u64,
}

fn main() -> std::process::ExitCode {
    let cli = Cli::parse();

    let Some(git_exe) = git::exe::resolve("git") else {
        eprintln!("找不到 git。这是硬依赖，请先安装或把它加进 PATH。");
        return std::process::ExitCode::from(2);
    };

    let mut cfg = ScanConfig {
        repos: cli.repo,
        seeds: cli.seed,
        idle: Duration::from_secs(cli.idle_hours * 3600),
        cache_quiet: Duration::from_secs(cli.cache_quiet_mins * 60),
        ..ScanConfig::default()
    };
    cfg.seeds.extend(discover::default_seeds());

    // 拿不到 gh 就走离线判定，而不是让整道 landed 门禁瘫痪。
    // 这里显式告知用户判据降级了——否则他会把"未进主干"误读成事实，
    // 而实际上只是没能力查证（squash-merge 的分支正是这样被误扣留 35GB 的）。
    let forge: Box<dyn wtgc::gates::MergeStatusProvider> = if cli.offline {
        Box::new(forge::Offline)
    } else {
        match forge::github::GhCli::detect() {
            Some(gh) => Box::new(gh),
            None => {
                eprintln!("提示：未找到 gh，squash-merge 的判定将降级为离线 diff。");
                Box::new(forge::Offline)
            }
        }
    };

    let env = Env {
        git: Box::new(git::RealGit::new(git_exe, cfg.git_timeout)),
        forge,
        clock: Box::new(SystemClock),
        procs: Box::new(platform::procs::SysinfoProcs),
    };

    let report = scan(&cfg, &env);

    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    let res = if cli.json {
        match report::json::render(&report) {
            Ok(s) => writeln!(out, "{s}"),
            Err(e) => {
                eprintln!("序列化报告失败: {e}");
                return std::process::ExitCode::from(1);
            }
        }
    } else {
        report::human::render(&report, &mut out)
    };

    if let Err(e) = res {
        // 管道被下游关掉（`wtgc | head`）不是错误，别拿它吓唬用户
        if e.kind() != std::io::ErrorKind::BrokenPipe {
            eprintln!("写出报告失败: {e}");
            return std::process::ExitCode::from(1);
        }
    }

    if report.repos.is_empty() {
        eprintln!(
            "没有发现任何仓库。用 --repo 指定，或确认种子目录下确实有 agent 建的 worktree。"
        );
        return std::process::ExitCode::from(2);
    }
    std::process::ExitCode::SUCCESS
}
