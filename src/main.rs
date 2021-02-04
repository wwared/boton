#![feature(async_closure)]

use log::*;

mod irc;
mod bot;
mod plugins;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    env_logger::init();

    let bots = bot::Config::load_from("config")?;
    println!("Loaded config: {:#?}", bots);
    let bot_handles = bots.spawn_tasks().await?;

    for h in bot_handles {
        h.await??;
    }

    info!("All connections closed, exiting...");
    Ok(())
}
