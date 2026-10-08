//! Wire layer for the Chrome DevTools Protocol.
//!
//! This crate knows nothing about CDP semantics. It only carries bytes:
//! HTTP discovery requests, the WebSocket upgrade handshake, RFC 6455 frame
//! framing, and the JSON envelope shape that CDP messages share.
//!
//! Everything above it (which methods exist, what they answer) lives in
//! `browser-core`.

pub mod http;
pub mod server;
pub mod ws;

pub use http::{Request, Response};
pub use server::{serve, Connection, Handler};
pub use ws::{opcode, Frame};
