use anyhow::{anyhow, Result};
use bytes::{Buf, BytesMut};
use log::*;
use nom::{character::complete::char, multi::many0, combinator::cond, bytes::complete::{take, take_till1}, IResult};
use std::convert::TryFrom;
use tokio::{io::{AsyncReadExt, AsyncWriteExt, BufWriter, ReadHalf, WriteHalf, split}, net::{ToSocketAddrs, TcpStream}, task::JoinHandle};
use tokio_native_tls::TlsConnector;

use tokio::sync::broadcast;
use tokio::sync::mpsc;

fn process_buf(src: &mut BytesMut) -> Vec<Message> {
    let mut res = vec![];
    let mut start = 0;
    for (pos, win) in src.windows(2).enumerate() {
        if win == b"\r\n" {
            let decoded = String::from_utf8_lossy(&src[start..pos]);
            debug!("<- \"{}\"", decoded);

            // FIXME: can't ? here
            let msg = parse_line(&decoded);
            if let Ok((_, msg)) = msg {
                res.push(msg);
            } else {
                error!("Parse failed for line: {}", decoded);
                error!("Error: {:?}", msg);
            }

            start = pos + 2;
        }
    }
    // trace!("Advancing buf by {}:\n{:?}", start, &src[..start]);
    src.advance(start);
    res
}

fn is_space(ch: char) -> bool {
    ch == ' '
}

fn starts_with_colon(input: &str) -> IResult<&str, bool> {
    let (input, has_colon) = cond(input.starts_with(':'), take(1usize))(input)?;
    trace!("starts_with_colon: {}", has_colon.is_some());
    Ok((input, has_colon.is_some()))
}

fn skip_space(input: &str) -> IResult<&str, ()> {
    let (input, spaces) = many0(char(' '))(input)?;
    trace!("skip_space: {}", spaces.len());
    Ok((input, ()))
}

fn parse_parameter(input: &str) -> IResult<&str, &str> {
    let (input, has_colon) = starts_with_colon(input)?;
    if has_colon {
        trace!("parse_parameter: has_colon, got '{}'", input);
        Ok(("", input))
    } else {
        let (input, param) = take_till1(is_space)(input)?;
        trace!("parse_parameter: no_colon, got '{}'", param);
        let (input, _) = skip_space(input)?;
        Ok((input, param))
    }
}

fn parse_line(input: &str) -> IResult<&str, Message> {
    let (input, has_source) = starts_with_colon(input)?;
    let (input, source) = if has_source {
        let (input, source) = take_till1(is_space)(input)?;
        let (input, _) = skip_space(input)?;
        trace!("got source: {}", source);
        (input, Some(source.into()))
    } else {
        trace!("no source, rest: {}", input);
        (input, None)
    };

    let (input, command) = take_till1(is_space)(input)?;
    trace!("got command text: {}", command);
    let command = Command::try_from(command).unwrap();
    trace!("parsed: {:?}", command);
    let (input, _) = skip_space(input)?;

    let (input, params) = many0(parse_parameter)(input)?;
    trace!("got params: {:?}", params);
    trace!("rest: {}", input);

    let target = if !params.is_empty() {
        Some(params[0].into())
    } else {
        None
    };
    trace!("target: {:?}", target);

    let parameters: Vec<String> = params[1..].iter().map(|s| s.to_string()).collect();
    trace!("params as strings: {:?}", parameters);

    Ok((input, Message {
        source,
        command,
        target,
        parameters,
    }))
}

impl<'a> TryFrom<&'a str> for Command {
    type Error = anyhow::Error;

    fn try_from(value: &'a str) -> Result<Self> {
        if value.is_empty() {
            return Err(anyhow!("empty string as command"));
        }
        match value {
            "PING" => Ok(Command::Ping),
            "NOTICE" => Ok(Command::Notice),
            "PRIVMSG" => Ok(Command::Privmsg),
            "001" => Ok(Command::RplWelcome),
            "433" => Ok(Command::ErrNicknameInUse),
            _ => {
                Ok(Command::Other(value.into()))
            }
        }
    }
}

impl TryFrom<&Command> for String {
    type Error = anyhow::Error;

    fn try_from(cmd: &Command) -> Result<Self> {
        match cmd {
            Command::Join => Ok("JOIN".into()),
            Command::Nick => Ok("NICK".into()),
            Command::Notice => Ok("NOTICE".into()),
            Command::Ping => Ok("PONG".into()),
            Command::Privmsg => Ok("PRIVMSG".into()),
            Command::Other(val) => Ok(val.clone()),

            Command::ErrNicknameInUse | Command::RplWelcome => {
                error!("Tried to send {:?} to server", cmd);
                Err(anyhow!("invalid command"))
            }
        }
    }
}

const READ_BUF_SIZE: usize = 4 * 1024;
const RECV_MSG_CHAN: usize = 16;
const SEND_MSG_CHAN: usize = 16;

// TODO remove allow(dead_code)
#[allow(dead_code)]
pub async fn connect<A: ToSocketAddrs>(server: &str, addr: A) -> Result<(IRC, JoinHandle<Result<()>>)> {
    let stream = TcpStream::connect(addr).await?;

    let conn = Connection::from_socket(server.into(), stream);
    conn.spawn_tasks().await
}

#[allow(dead_code)]
pub async fn connect_tls<A: ToSocketAddrs>(server: &str, addr: A, domain: &str) -> Result<(IRC, JoinHandle<Result<()>>)> {
    let connector = tokio_native_tls::native_tls::TlsConnector::builder()
        .danger_accept_invalid_certs(true)
        .use_sni(false)
        .build()?;
    let connector = TlsConnector::from(connector);

    let stream = TcpStream::connect(addr).await?;

    let stream = connector.connect(domain, stream).await?;

    let conn = Connection::from_socket(server.into(), stream);
    conn.spawn_tasks().await
}

impl<S: 'static + AsyncReadExt + AsyncWriteExt + Unpin + Send> Connection<S> {
    fn from_socket(server: String, socket: S) -> Self {
        let (recv_half, write_half) = split(socket);
        let write_half = BufWriter::new(write_half);
        let recv_buffer: BytesMut = BytesMut::with_capacity(READ_BUF_SIZE);
        let (received_messages, rx) = broadcast::channel(RECV_MSG_CHAN);
        drop(rx);
        let sent_messages = mpsc::channel(SEND_MSG_CHAN);
        Self {
            server, write_half, recv_half, recv_buffer, received_messages, sent_messages
        }
    }

    async fn send_message(stream: &mut BufWriter<WriteHalf<S>>, msg: &Message) -> Result<()> {
        trace!("Sending message: {:?}", msg);
        let cmd = String::try_from(&msg.command)?;
        stream.write_all(cmd.as_bytes()).await?;

        if let Some(target) = &msg.target {
            stream.write_all(b" ").await?;
            if msg.parameters.is_empty() && target.contains(' ') {
                stream.write_all(b":").await?;
            }
            stream.write_all(target.as_bytes()).await?;
        }

        if !msg.parameters.is_empty() {
            for (idx, param) in msg.parameters.iter().enumerate() {
                stream.write_all(b" ").await?;
                if idx == msg.parameters.len()-1 {
                    stream.write_all(b":").await?;
                }
                stream.write_all(param.as_bytes()).await?;
            }
        }

        stream.write_all(b"\r\n").await?;
        debug!("-> {:?}", String::from_utf8_lossy(stream.buffer()));
        stream.flush().await?;
        Ok(())
    }

    async fn spawn_tasks(self) -> Result<(IRC, JoinHandle<Result<()>>)> {
        trace!("Spawning connection tasks...");
        let irc = self.get_channels();
        let join_handle = tokio::spawn(async move {
            let (mut send_channel_rx, mut write_half) = (self.sent_messages.1, self.write_half);

            let (recv_channel_tx, mut recv_half, mut recv_buffer) = (self.received_messages, self.recv_half, self.recv_buffer);

            // Read messages
            let read_handle = tokio::spawn(async move {
                loop {
                    Connection::receive_messages(&mut recv_half, &mut recv_buffer, &recv_channel_tx).await.unwrap();
                    trace!("Processed a batch of received messages");
                }
            });
            trace!("Spawned read task: {:?}", read_handle);

            // Send messages
            let send_handle = tokio::spawn(async move {
                while let Some(msg) = send_channel_rx.recv().await {
                    trace!("Got message to send");
                    Connection::send_message(&mut write_half, &msg).await.unwrap();
                }
            });
            trace!("Spawned send task: {:?}", send_handle);

            read_handle.await?;
            send_handle.await?;
            warn!("Exiting connection tasks...");
            Ok(())
        });
        Ok((irc, join_handle))
    }

    async fn receive_messages(stream: &mut ReadHalf<S>, buffer: &mut BytesMut, recv_messages_tx: &broadcast::Sender<Message>) -> Result<()> {
        if stream.read_buf(buffer).await? == 0 {
            if buffer.is_empty() {
                error!("closed connection by peer");
                return Err(anyhow!("closed connection by peer"));
            } else {
                panic!("unread data in read buffer");
            }
        }

        let messages = process_buf(buffer);
        for msg in messages {
            recv_messages_tx.send(msg)?;
        }

        Ok(())
    }

    fn get_channels(&self) -> IRC {
        IRC {
            server: self.server.clone(),
            received_messages_sender: self.received_messages.clone(),
            received_messages: self.received_messages.subscribe(),
            send_messages: self.sent_messages.0.clone(),
        }
    }
}

impl Message {
    fn single_argument<S: Into<String>>(cmd: Command, arg: S) -> Message {
        Message {
            source: None,
            command: cmd,
            target: Some(arg.into()),
            parameters: Vec::with_capacity(0),
        }
    }

    fn double_argument<S: Into<String>>(cmd: Command, target: S, arg: S) -> Message {
        Message {
            source: None,
            command: cmd,
            target: Some(target.into()),
            parameters: vec![arg.into()],
        }
    }

    fn nick<S: Into<String>>(new_nick: S) -> Message {
        Message::single_argument(Command::Nick, new_nick)
    }

    fn join<S: Into<String>>(channel: S) -> Message {
        Message::single_argument(Command::Join, channel)
    }

    pub fn privmsg<S: Into<String>>(target: S, message: S) -> Message {
        Message::double_argument(Command::Privmsg, target, message)
    }

    pub fn source_as_user(&self) -> Option<User> {
        if let Some(src) = self.source.clone() {
            if let Some(bang) = src.find('!') {
                if let Some(at) = src[bang..].find('@') {
                    Some(User {
                        nick: src[..bang].to_string(),
                        ident: src[bang+1..bang+at].to_string(),
                        host: src[bang+at+1..].to_string(),
                    })
                } else {
                    None
                }
            } else {
                None
            }
        } else {
            None
        }
    }
}

impl IRC {
    // TODO probably move these out of this file?
    pub async fn authenticate(&mut self, nick: String, ident: String, real_name: String) -> Result<()> {
        self.send_messages.send(Message {
            source: None,
            command: Command::Other("USER".into()),
            target: None,
            parameters: vec![ident, "0".into(), "*".into(), real_name],
        }).await?;
        self.send_messages.send(Message::nick(nick)).await?;
        Ok(())
    }

    pub async fn join(&mut self, channels: &[String]) -> Result<()> {
        for ch in channels {
            self.send_messages.send(Message::join(ch)).await?;
        }
        Ok(())
    }

    pub async fn reply_pong(&mut self, msg: Message) -> Result<()> {
        self.send_messages.send(msg).await?;
        Ok(())
    }

    pub async fn reply_nick_in_use(&mut self, msg: Message) -> Result<()> {
        assert!(msg.command == Command::ErrNicknameInUse);
        assert!(!msg.parameters.is_empty());
        self.send_messages.send(Message::nick(format!("{}_", msg.parameters[0]))).await?;
        Ok(())
    }
}

/// Type exposed to users for receiving and sending messages.
pub struct IRC {
    pub server: String,

    received_messages_sender: broadcast::Sender<Message>,
    pub received_messages: broadcast::Receiver<Message>,
    pub send_messages: mpsc::Sender<Message>,
}

impl Clone for IRC {
    fn clone(&self) -> IRC {
        IRC {
            server: self.server.clone(),
            received_messages_sender: self.received_messages_sender.clone(),
            received_messages: self.received_messages_sender.subscribe(),
            send_messages: self.send_messages.clone(),
        }
    }
}

/// Inner type used by tasks for reading and writing to the underlying socket S.
struct Connection<S> {
    server: String,

    write_half: BufWriter<WriteHalf<S>>,
    recv_half: ReadHalf<S>,
    recv_buffer: BytesMut,

    received_messages: broadcast::Sender<Message>,
    sent_messages: (mpsc::Sender<Message>, mpsc::Receiver<Message>),
}

/// Type identifying a single user.
#[derive(Debug)]
pub struct User {
    pub nick: String,
    pub ident: String,
    pub host: String,
}

/// Type describing single IRC message.
#[derive(Clone, Debug)]
pub struct Message {
    pub source: Option<String>,
    pub command: Command,
    pub target: Option<String>,
    pub parameters: Vec<String>,
}

/// List of recognized IRC commands.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Command {
    Join,
    Nick,
    Notice,
    Privmsg,
    Ping,
    RplWelcome,
    ErrNicknameInUse,
    Other(String),
}
