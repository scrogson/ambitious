//! Simple text-based chat client.
//!
//! This client provides a human-readable text interface while
//! communicating with the server using the binary protocol.
//!
//! # Usage
//!
//! ```bash
//! cargo run --bin chat-client
//! ```
//!
//! # Commands
//!
//! - `/nick <name>` - Set your nickname
//! - `/join <room>` - Join a room
//! - `/leave <room>` - Leave a room
//! - `/msg <room> <text>` - Send a message to a room
//! - `/rooms` - List all rooms
//! - `/users <room>` - List users in a room
//! - `/quit` - Disconnect

use std::env;
use std::io::{self, BufRead, Write};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

// Include the protocol module from the main crate
#[path = "../protocol.rs"]
mod protocol;

use protocol::{frame_message, parse_frame, ClientCommand, ServerEvent};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let addr = env::var("CHAT_SERVER").unwrap_or_else(|_| "127.0.0.1:9999".to_string());

    println!("Connecting to {}...", addr);
    let stream = TcpStream::connect(&addr).await?;
    println!("Connected! Type /help for commands.\n");

    let (mut reader, mut writer) = stream.into_split();

    // Spawn task to read from server
    let read_handle = tokio::spawn(async move {
        let mut buf = vec![0u8; 4096];
        let mut pending = Vec::new();

        loop {
            match reader.read(&mut buf).await {
                Ok(0) => {
                    println!("\n[Disconnected from server]");
                    break;
                }
                Ok(n) => {
                    pending.extend_from_slice(&buf[..n]);

                    // Parse and display all complete messages
                    while let Some((event, consumed)) = parse_frame::<ServerEvent>(&pending) {
                        pending.drain(..consumed);
                        display_event(&event);
                    }
                }
                Err(e) => {
                    eprintln!("\n[Read error: {}]", e);
                    break;
                }
            }
        }
    });

    // Read from stdin and send to server
    let stdin = io::stdin();
    let mut stdout = io::stdout();

    for line in stdin.lock().lines() {
        let line = line?;
        let line = line.trim();

        if line.is_empty() {
            continue;
        }

        let cmd = match parse_input(line) {
            Some(cmd) => cmd,
            None => {
                println!("[Unknown command. Type /help for help]");
                continue;
            }
        };

        // Check for local commands
        if matches!(line, "/help" | "/h" | "/?") {
            print_help();
            continue;
        }

        // Send command to server
        let frame = frame_message(&cmd);
        if let Err(e) = writer.write_all(&frame).await {
            eprintln!("[Send error: {}]", e);
            break;
        }

        // Quit command
        if matches!(cmd, ClientCommand::Quit) {
            println!("[Goodbye!]");
            break;
        }

        print!("> ");
        stdout.flush()?;
    }

    read_handle.abort();
    Ok(())
}

fn parse_input(input: &str) -> Option<ClientCommand> {
    if !input.starts_with('/') {
        // Not a command - treat as error for now
        // In a real client, you might have a "current room" for quick messaging
        return None;
    }

    let parts: Vec<&str> = input[1..].splitn(3, ' ').collect();
    let cmd = parts.first()?;

    match *cmd {
        "nick" | "n" => {
            let nick = parts.get(1)?;
            Some(ClientCommand::Nick(nick.to_string()))
        }
        "join" | "j" => {
            let room = parts.get(1)?;
            Some(ClientCommand::Join(room.to_string()))
        }
        "leave" | "l" => {
            let room = parts.get(1)?;
            Some(ClientCommand::Leave(room.to_string()))
        }
        "msg" | "m" => {
            let room = parts.get(1)?;
            let text = parts.get(2)?;
            Some(ClientCommand::Msg {
                room: room.to_string(),
                text: text.to_string(),
            })
        }
        "rooms" | "r" => Some(ClientCommand::ListRooms),
        "users" | "u" => {
            let room = parts.get(1)?;
            Some(ClientCommand::ListUsers(room.to_string()))
        }
        "quit" | "q" | "exit" => Some(ClientCommand::Quit),
        "help" | "h" | "?" => None, // Handled locally
        _ => None,
    }
}

fn display_event(event: &ServerEvent) {
    match event {
        ServerEvent::Welcome { message } => {
            println!("\n{}", message);
            println!();
        }
        ServerEvent::NickOk { nick } => {
            println!("[Nickname set to '{}']", nick);
        }
        ServerEvent::NickError { reason } => {
            println!("[Nick error: {}]", reason);
        }
        ServerEvent::Joined { room } => {
            println!("[Joined #{}]", room);
        }
        ServerEvent::JoinError { room, reason } => {
            println!("[Failed to join #{}: {}]", room, reason);
        }
        ServerEvent::Left { room } => {
            println!("[Left #{}]", room);
        }
        ServerEvent::Message { room, from, text } => {
            println!("[#{}] <{}> {}", room, from, text);
        }
        ServerEvent::UserJoined { room, nick } => {
            println!("[#{}] {} joined", room, nick);
        }
        ServerEvent::UserLeft { room, nick } => {
            println!("[#{}] {} left", room, nick);
        }
        ServerEvent::RoomList { rooms } => {
            if rooms.is_empty() {
                println!("[No rooms]");
            } else {
                println!("[Rooms:]");
                for room in rooms {
                    println!("  #{} ({} users)", room.name, room.user_count);
                }
            }
        }
        ServerEvent::UserList { room, users } => {
            if users.is_empty() {
                println!("[#{} has no users]", room);
            } else {
                println!("[Users in #{}:]", room);
                for user in users {
                    println!("  {}", user);
                }
            }
        }
        ServerEvent::Error { message } => {
            println!("[Error: {}]", message);
        }
        ServerEvent::Shutdown => {
            println!("[Server shutting down]");
        }
    }
}

fn print_help() {
    println!(
        r#"
Commands:
  /nick <name>         Set your nickname
  /join <room>         Join a room (creates if needed)
  /leave <room>        Leave a room
  /msg <room> <text>   Send a message to a room
  /rooms               List all rooms
  /users <room>        List users in a room
  /quit                Disconnect

Short forms: /n, /j, /l, /m, /r, /u, /q
"#
    );
}
