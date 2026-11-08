//! Manual live-account check: quota fields only, no transcript scan.
use aitop::{
    config::Paths,
    models::{Provider, Result},
    parser::{self, FetchOptions},
};
use clap::Parser;
use std::sync::atomic::AtomicBool;

#[derive(Parser)]
struct Args {
    #[arg(long, value_enum)]
    provider: Option<Provider>,
    #[arg(long)]
    cli: bool,
}

fn main() -> Result<()> {
    let args = Args::parse();
    let home = dirs::home_dir().ok_or("Cannot find home directory")?;
    let paths = Paths::new(home, None);
    let options = FetchOptions {
        keychain: true,
        cli: args.cli,
        agy_cli: false,
        offline: false,
        force_refresh: false,
    };
    let quotas = std::thread::scope(|scope| {
        let providers = args
            .provider
            .map(|p| vec![p])
            .unwrap_or_else(|| vec![Provider::Codex, Provider::Claude]);
        let workers = providers
            .into_iter()
            .map(|provider| {
                scope.spawn({
                    let paths = &paths;
                    move || {
                        parser::fetch(
                            provider,
                            paths,
                            options,
                            &AtomicBool::new(false),
                            chrono::Local::now().timestamp(),
                        )
                    }
                })
            })
            .collect::<Vec<_>>();
        workers
            .into_iter()
            .map(|w| w.join().expect("quota worker"))
            .collect::<Vec<_>>()
    });
    println!("{}", serde_json::to_string_pretty(&quotas)?);
    Ok(())
}
