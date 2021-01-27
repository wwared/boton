use anyhow::Result;
use tokio::task::JoinHandle;

use crate::irc;

pub mod echo;

pub trait Plugin {
    fn new() -> Self;

    fn spawn_task(self, irc: irc::IRC) -> Result<JoinHandle<Result<()>>>;
}
