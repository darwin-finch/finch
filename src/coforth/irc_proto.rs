/// IRC-like binary channel protocol.
///
/// Wire format per message:
///   [u8:  op          ]
///   [u16: from_len    ][from_bytes   ]
///   [u16: chan_len    ][chan_bytes   ]
///   [u32: body_len   ][body_bytes   ]
///
/// All integers are little-endian.  Strings are UTF-8.
///
/// Opcodes:
///   0x01  JOIN   — nick joins a channel
///   0x02  PART   — nick leaves a channel
///   0x03  SAY    — plain text message to channel
///   0x04  YIELD  — contribute Forth code to channel stack
///   0x05  EXEC   — execute accumulated channel stack; reply carries combined code
///   0x06  NAMES  — request member list; reply is SAY with comma-separated nicks
///   0x07  ACK    — acknowledgement; body is the original opcode as a single byte
///   0x08  ERR    — error; body is a UTF-8 error string

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

pub const OP_JOIN: u8 = 0x01;
pub const OP_PART: u8 = 0x02;
pub const OP_SAY: u8 = 0x03;
pub const OP_YIELD: u8 = 0x04;
pub const OP_EXEC: u8 = 0x05;
pub const OP_NAMES: u8 = 0x06;
pub const OP_ACK: u8 = 0x07;
pub const OP_ERR: u8 = 0x08;
/// Execute only the most recent contribution, leaving the rest of the stack intact.
pub const OP_EXEC_LAST: u8 = 0x09;

/// A single message on the wire.
#[derive(Debug, Clone, PartialEq)]
pub struct IrcMessage {
    pub op: u8,
    pub from: String,
    pub chan: String,
    pub body: Vec<u8>,
}

impl IrcMessage {
    pub fn new(
        op: u8,
        from: impl Into<String>,
        chan: impl Into<String>,
        body: impl Into<Vec<u8>>,
    ) -> Self {
        Self {
            op,
            from: from.into(),
            chan: chan.into(),
            body: body.into(),
        }
    }

    /// Encode to bytes.
    pub fn encode(&self) -> Vec<u8> {
        let from = self.from.as_bytes();
        let chan = self.chan.as_bytes();
        let body = &self.body;
        let cap = 1 + 2 + from.len() + 2 + chan.len() + 4 + body.len();
        let mut buf = Vec::with_capacity(cap);
        buf.push(self.op);
        buf.extend_from_slice(&(from.len() as u16).to_le_bytes());
        buf.extend_from_slice(from);
        buf.extend_from_slice(&(chan.len() as u16).to_le_bytes());
        buf.extend_from_slice(chan);
        buf.extend_from_slice(&(body.len() as u32).to_le_bytes());
        buf.extend_from_slice(body);
        buf
    }

    /// Decode from a byte slice.  Returns `(message, bytes_consumed)` or `None`
    /// if the buffer is incomplete.  Never panics on truncated input.
    pub fn decode(buf: &[u8]) -> Option<(Self, usize)> {
        let mut pos = 0;

        if buf.len() < pos + 1 {
            return None;
        }
        let op = buf[pos];
        pos += 1;

        if buf.len() < pos + 2 {
            return None;
        }
        let from_len = u16::from_le_bytes([buf[pos], buf[pos + 1]]) as usize;
        pos += 2;
        if buf.len() < pos + from_len {
            return None;
        }
        let from = String::from_utf8_lossy(&buf[pos..pos + from_len]).into_owned();
        pos += from_len;

        if buf.len() < pos + 2 {
            return None;
        }
        let chan_len = u16::from_le_bytes([buf[pos], buf[pos + 1]]) as usize;
        pos += 2;
        if buf.len() < pos + chan_len {
            return None;
        }
        let chan = String::from_utf8_lossy(&buf[pos..pos + chan_len]).into_owned();
        pos += chan_len;

        if buf.len() < pos + 4 {
            return None;
        }
        let body_len =
            u32::from_le_bytes([buf[pos], buf[pos + 1], buf[pos + 2], buf[pos + 3]]) as usize;
        pos += 4;
        if buf.len() < pos + body_len {
            return None;
        }
        let body = buf[pos..pos + body_len].to_vec();
        pos += body_len;

        Some((Self { op, from, chan, body }, pos))
    }
}

/// Per-channel state: member list + accumulated yield stack.
#[derive(Debug, Default)]
pub struct ChannelState {
    /// Nicks currently in the channel.
    pub members: Vec<String>,
    /// Accumulated `(from, code)` contributions from YIELD messages.
    pub stack: Vec<(String, String)>,
}

/// Shared channel registry — all active channels on this node.
pub type ChannelRegistry = Arc<Mutex<HashMap<String, ChannelState>>>;

pub fn new_channel_registry() -> ChannelRegistry {
    Arc::new(Mutex::new(HashMap::new()))
}

/// Start the IRC-like binary TCP channel server on `port`.
/// Runs until the process exits or the listener errors fatally.
pub async fn start_channel_server(port: u16, registry: ChannelRegistry) -> anyhow::Result<()> {
    let listener = TcpListener::bind(format!("0.0.0.0:{port}")).await?;
    tracing::info!("IRC-like channel server listening on :{port}");
    loop {
        match listener.accept().await {
            Ok((stream, peer_addr)) => {
                let reg = Arc::clone(&registry);
                tokio::spawn(async move {
                    if let Err(e) = handle_peer(stream, reg).await {
                        tracing::debug!("channel peer {peer_addr} disconnected: {e}");
                    }
                });
            }
            Err(e) => {
                tracing::warn!("channel server accept error: {e}");
            }
        }
    }
}

async fn handle_peer(mut stream: TcpStream, registry: ChannelRegistry) -> anyhow::Result<()> {
    let mut buf: Vec<u8> = Vec::new();
    let mut tmp = [0u8; 4096];
    loop {
        let n = stream.read(&mut tmp).await?;
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&tmp[..n]);

        while let Some((msg, consumed)) = IrcMessage::decode(&buf) {
            buf.drain(..consumed);
            if let Some(reply) = process_message(msg, &registry) {
                stream.write_all(&reply.encode()).await?;
            }
        }
    }
    Ok(())
}

/// Pure message processing — no I/O.  Returns an optional reply message.
pub fn process_message(msg: IrcMessage, registry: &ChannelRegistry) -> Option<IrcMessage> {
    let mut chans = registry.lock().unwrap();
    let state = chans.entry(msg.chan.clone()).or_default();

    match msg.op {
        OP_JOIN => {
            if !state.members.contains(&msg.from) {
                state.members.push(msg.from.clone());
            }
            Some(IrcMessage::new(OP_ACK, "server", &msg.chan, [OP_JOIN]))
        }
        OP_PART => {
            state.members.retain(|m| m != &msg.from);
            Some(IrcMessage::new(OP_ACK, "server", &msg.chan, [OP_PART]))
        }
        OP_SAY => Some(IrcMessage::new(OP_SAY, "server", &msg.chan, msg.body)),
        OP_YIELD => {
            let code = String::from_utf8_lossy(&msg.body).into_owned();
            state.stack.push((msg.from.clone(), code));
            Some(IrcMessage::new(OP_ACK, "server", &msg.chan, [OP_YIELD]))
        }
        OP_EXEC => {
            let combined: String = state
                .stack
                .iter()
                .map(|(_, c)| c.as_str())
                .collect::<Vec<_>>()
                .join(" ");
            state.stack.clear();
            // Reply carries the combined Forth code; caller runs it locally.
            Some(IrcMessage::new(
                OP_EXEC,
                "server",
                &msg.chan,
                combined.as_bytes().to_vec(),
            ))
        }
        OP_EXEC_LAST => {
            // Pop only the most recent contribution; leave everything else.
            let last = state.stack.pop();
            let code = last.map(|(_, c)| c).unwrap_or_default();
            Some(IrcMessage::new(
                OP_EXEC_LAST,
                "server",
                &msg.chan,
                code.as_bytes().to_vec(),
            ))
        }
        OP_NAMES => {
            let names = state.members.join(",");
            Some(IrcMessage::new(
                OP_SAY,
                "server",
                &msg.chan,
                names.as_bytes().to_vec(),
            ))
        }
        _ => Some(IrcMessage::new(
            OP_ERR,
            "server",
            &msg.chan,
            format!("unknown op 0x{:02x}", msg.op).as_bytes().to_vec(),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── codec ──────────────────────────────────────────────────────────────

    #[test]
    fn test_irc_encode_decode_round_trip() {
        let msg = IrcMessage::new(OP_SAY, "alice", "#general", b"hello world".to_vec());
        let enc = msg.encode();
        let (dec, n) = IrcMessage::decode(&enc).unwrap();
        assert_eq!(dec, msg);
        assert_eq!(n, enc.len());
    }

    #[test]
    fn test_irc_decode_partial_returns_none() {
        let msg = IrcMessage::new(OP_JOIN, "bob", "#test", b"".to_vec());
        let enc = msg.encode();
        assert!(IrcMessage::decode(&enc[..enc.len() - 1]).is_none());
    }

    #[test]
    fn test_irc_decode_empty_buffer_returns_none() {
        assert!(IrcMessage::decode(&[]).is_none());
    }

    #[test]
    fn test_irc_encode_decode_empty_body() {
        let msg = IrcMessage::new(OP_PART, "carol", "#lobby", b"".to_vec());
        let enc = msg.encode();
        let (dec, _) = IrcMessage::decode(&enc).unwrap();
        assert_eq!(dec.op, OP_PART);
        assert!(dec.body.is_empty());
    }

    #[test]
    fn test_irc_encode_decode_binary_body() {
        let body: Vec<u8> = (0u8..=255).collect();
        let msg = IrcMessage::new(OP_YIELD, "node1", "#bin", body.clone());
        let enc = msg.encode();
        let (dec, _) = IrcMessage::decode(&enc).unwrap();
        assert_eq!(dec.body, body);
    }

    #[test]
    fn test_irc_decode_two_messages_sequential() {
        let m1 = IrcMessage::new(OP_JOIN, "a", "#x", b"".to_vec());
        let m2 = IrcMessage::new(OP_SAY, "b", "#x", b"hi".to_vec());
        let mut buf = m1.encode();
        buf.extend(m2.encode());

        let (d1, n1) = IrcMessage::decode(&buf).unwrap();
        assert_eq!(d1.op, OP_JOIN);
        let (d2, _) = IrcMessage::decode(&buf[n1..]).unwrap();
        assert_eq!(d2.op, OP_SAY);
    }

    // ── channel state ──────────────────────────────────────────────────────

    #[test]
    fn test_channel_join_adds_member() {
        let reg = new_channel_registry();
        process_message(IrcMessage::new(OP_JOIN, "alice", "#test", b"".to_vec()), &reg);
        let chans = reg.lock().unwrap();
        assert!(chans["#test"].members.contains(&"alice".to_string()));
    }

    #[test]
    fn test_channel_join_idempotent() {
        let reg = new_channel_registry();
        process_message(IrcMessage::new(OP_JOIN, "alice", "#test", b"".to_vec()), &reg);
        process_message(IrcMessage::new(OP_JOIN, "alice", "#test", b"".to_vec()), &reg);
        let chans = reg.lock().unwrap();
        assert_eq!(chans["#test"].members.iter().filter(|m| *m == "alice").count(), 1);
    }

    #[test]
    fn test_channel_part_removes_member() {
        let reg = new_channel_registry();
        process_message(IrcMessage::new(OP_JOIN, "alice", "#test", b"".to_vec()), &reg);
        process_message(IrcMessage::new(OP_PART, "alice", "#test", b"".to_vec()), &reg);
        let chans = reg.lock().unwrap();
        assert!(!chans["#test"].members.contains(&"alice".to_string()));
    }

    #[test]
    fn test_channel_yield_accumulates() {
        let reg = new_channel_registry();
        process_message(IrcMessage::new(OP_YIELD, "alice", "#test", b"1 2 +".to_vec()), &reg);
        process_message(IrcMessage::new(OP_YIELD, "bob", "#test", b"3 *".to_vec()), &reg);
        let chans = reg.lock().unwrap();
        assert_eq!(chans["#test"].stack.len(), 2);
    }

    #[test]
    fn test_channel_exec_returns_combined_and_clears_stack() {
        let reg = new_channel_registry();
        process_message(IrcMessage::new(OP_YIELD, "a", "#x", b"1 2 +".to_vec()), &reg);
        process_message(IrcMessage::new(OP_YIELD, "b", "#x", b"dup".to_vec()), &reg);
        let reply =
            process_message(IrcMessage::new(OP_EXEC, "a", "#x", b"".to_vec()), &reg).unwrap();
        assert_eq!(reply.op, OP_EXEC);
        assert_eq!(String::from_utf8(reply.body).unwrap(), "1 2 + dup");
        let chans = reg.lock().unwrap();
        assert!(chans["#x"].stack.is_empty());
    }

    #[test]
    fn test_channel_exec_empty_stack_returns_empty_body() {
        let reg = new_channel_registry();
        let reply =
            process_message(IrcMessage::new(OP_EXEC, "a", "#empty", b"".to_vec()), &reg).unwrap();
        assert_eq!(reply.op, OP_EXEC);
        assert!(reply.body.is_empty());
    }

    #[test]
    fn test_channel_names_lists_members() {
        let reg = new_channel_registry();
        process_message(IrcMessage::new(OP_JOIN, "alice", "#test", b"".to_vec()), &reg);
        process_message(IrcMessage::new(OP_JOIN, "bob", "#test", b"".to_vec()), &reg);
        let reply =
            process_message(IrcMessage::new(OP_NAMES, "", "#test", b"".to_vec()), &reg).unwrap();
        assert_eq!(reply.op, OP_SAY);
        let names = String::from_utf8(reply.body).unwrap();
        assert!(names.contains("alice"));
        assert!(names.contains("bob"));
    }

    #[test]
    fn test_channel_join_ack_opcode() {
        let reg = new_channel_registry();
        let reply =
            process_message(IrcMessage::new(OP_JOIN, "x", "#y", b"".to_vec()), &reg).unwrap();
        assert_eq!(reply.op, OP_ACK);
        assert_eq!(reply.body, [OP_JOIN]);
    }

    #[test]
    fn test_channel_unknown_op_returns_err() {
        let reg = new_channel_registry();
        let reply = process_message(IrcMessage::new(0xFF, "x", "#y", b"".to_vec()), &reg).unwrap();
        assert_eq!(reply.op, OP_ERR);
    }

    #[test]
    fn test_exec_last_pops_only_most_recent() {
        let reg = new_channel_registry();
        process_message(IrcMessage::new(OP_YIELD, "a", "#x", b"first".to_vec()), &reg);
        process_message(IrcMessage::new(OP_YIELD, "b", "#x", b"last".to_vec()), &reg);
        let reply =
            process_message(IrcMessage::new(OP_EXEC_LAST, "a", "#x", b"".to_vec()), &reg)
                .unwrap();
        assert_eq!(reply.op, OP_EXEC_LAST);
        assert_eq!(String::from_utf8(reply.body).unwrap(), "last");
        // "first" must still be on the stack
        let chans = reg.lock().unwrap();
        assert_eq!(chans["#x"].stack.len(), 1);
        assert_eq!(chans["#x"].stack[0].1, "first");
    }

    #[test]
    fn test_exec_last_on_empty_channel_returns_empty() {
        let reg = new_channel_registry();
        let reply =
            process_message(IrcMessage::new(OP_EXEC_LAST, "a", "#empty", b"".to_vec()), &reg)
                .unwrap();
        assert_eq!(reply.op, OP_EXEC_LAST);
        assert!(reply.body.is_empty());
    }

    #[test]
    fn test_channel_independent_channels_dont_mix() {
        let reg = new_channel_registry();
        process_message(IrcMessage::new(OP_YIELD, "a", "#chan1", b"code1".to_vec()), &reg);
        process_message(IrcMessage::new(OP_YIELD, "b", "#chan2", b"code2".to_vec()), &reg);
        let chans = reg.lock().unwrap();
        assert_eq!(chans["#chan1"].stack.len(), 1);
        assert_eq!(chans["#chan2"].stack.len(), 1);
        assert_eq!(chans["#chan1"].stack[0].1, "code1");
        assert_eq!(chans["#chan2"].stack[0].1, "code2");
    }
}
