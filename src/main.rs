mod irc;
mod bot;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    env_logger::init();

    // TODO make some api for "plugins"

    let bots = bot::Config::load_from("config")?;
    println!("Loaded config: {:#?}", bots);
    let bot_handles = bots.spawn_tasks().await?;

    for handle in bot_handles {
        handle.await??;
    }

    // testing code for flooding a channel
    // let lol = irc.send_messages.clone();
    // tokio::spawn(async move {
    //     loop {
    //         lol.send(irc::Message {
    //             source: None,
    //             command: irc::Command::Other("PRIVMSG".into()),
    //             target: Some("#test".into()),
    //             parameters: vec![
    //                 "throughput testing".into()
    //             ],
    //         }).await.unwrap();
    //     }
    // });

    Ok(())
}
