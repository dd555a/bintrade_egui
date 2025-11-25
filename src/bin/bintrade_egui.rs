use anyhow::Result;
use bintrade_egui::client::cli_run;
use tracing_subscriber::{FmtSubscriber, EnvFilter};

fn main() ->Result<()> {
    let _subscriber = FmtSubscriber::builder()
        .with_env_filter(EnvFilter::from_default_env())
        .with_target(true)
        .pretty()
        .finish();
    env_logger::init();
    color_backtrace::install();
    cli_run()?;
    Ok(())
}
