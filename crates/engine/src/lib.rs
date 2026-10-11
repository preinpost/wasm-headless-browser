//! The real engine.
//!
//! M0 answered the protocol from a fixed page model in `engine-fake`. This
//! crate replaces that model with a real one: HTML goes in, a tree comes out,
//! and the accessibility tree is derived from that tree.
//!
//! What is here so far:
//!
//!   * [`dom`] — an arena DOM, `Send`, with index handles
//!   * [`parse`] — `html5ever` driven into that DOM
//!   * [`ax`] — the accessibility tree derived from it
//!   * [`fetch`] — plain HTTP by hand, plus `data:` URLs
//!   * [`select`] — the small CSS subset clients actually send
//!   * [`js`] — QuickJS, for real page script
//!   * [`browser`] — the `Engine` implementation that answers CDP from it
//!
//! Still to come: the `Engine` implementation that answers CDP from this tree,
//! fetching, layout, and a JavaScript engine.

pub mod ax;
pub mod bindings;
pub mod color;
pub mod browser;
pub mod cookies;
pub mod css;
pub mod dom;
pub mod encoding;
pub mod fetch;
pub mod font;
pub mod images;
pub mod js;
pub mod layout;
pub mod paint;
pub mod parse;
pub mod png;
pub mod modules;
pub mod network;
pub mod screencast;
pub mod scroll;
pub mod secrets;
pub mod select;
pub mod style;
pub mod text;
#[cfg(test)]
mod test_server;

pub use ax::AxNode;
pub use browser::HtmlEngine;
pub use dom::{Document, NodeId};
pub use parse::parse_html;
