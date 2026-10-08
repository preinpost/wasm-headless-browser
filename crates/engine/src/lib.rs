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
//!   * [`browser`] — the `Engine` implementation that answers CDP from it
//!
//! Still to come: the `Engine` implementation that answers CDP from this tree,
//! fetching, layout, and a JavaScript engine.

pub mod ax;
pub mod browser;
pub mod dom;
pub mod fetch;
pub mod parse;
pub mod select;

pub use ax::AxNode;
pub use browser::HtmlEngine;
pub use dom::{Document, NodeId};
pub use parse::parse_html;
