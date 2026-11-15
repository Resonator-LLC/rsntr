//! Write Resonator wave apps in Rust.
//!
//! An app is a WebAssembly component of the world `wave-app`
//! (`crates/resonator-apps/wit/app.wit`), built for `wasm32-wasip2` as a
//! `cdylib`. Implement [`Guest`] and name the type with [`export!`]:
//!
//! ```ignore
//! use resonator_app_sdk::{export, Guest, View, ui, wave};
//!
//! struct App;
//! impl Guest for App {
//!     fn render(_view: u32) -> Vec<resonator_app_sdk::UiNode> {
//!         let mut v = View::new();
//!         let row = v.el(None, "div").class("app-row").id();
//!         v.el(Some(row), "button").text("+1").on("click");
//!         v.finish()
//!     }
//!     // ... the other entry points
//! }
//! export!(App);
//! ```
//!
//! The imports are the modules [`wave`], [`kv`], [`peers`], [`ui`] and
//! [`log`]; each asks for a permission of the manifest, and fails without
//! it.

#[doc(hidden)]
#[allow(clippy::too_many_arguments)]
pub mod bindings {
    wit_bindgen::generate!({
        path: "../resonator-apps/wit",
        world: "wave-app",
        pub_export_macro: true,
        default_bindings_module: "resonator_app_sdk::bindings",
    });
}

pub use bindings::export;
pub use bindings::resonator::app::{kv, log, peers, ui, wave};
pub use bindings::{Guest, Peer, StreamMode, UiEvent, UiNode, WaveMessage};

/// Builds a rendered view: the flat list of nodes, each naming its parent.
#[derive(Debug, Default, Clone)]
pub struct View {
    nodes: Vec<UiNode>,
}

/// A node being built; [`El::id`] is its index, for children and events.
pub struct El<'a> {
    view: &'a mut View,
    index: u32,
}

impl View {
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds an element under `parent` (`None` for the top).
    pub fn el(&mut self, parent: Option<u32>, tag: &str) -> El<'_> {
        let index = self.nodes.len() as u32;
        self.nodes.push(UiNode {
            parent,
            tag: tag.to_owned(),
            text: None,
            attrs: Vec::new(),
            on: Vec::new(),
        });
        El { view: self, index }
    }

    /// The nodes, for `render`.
    pub fn finish(self) -> Vec<UiNode> {
        self.nodes
    }
}

impl El<'_> {
    fn node(&mut self) -> &mut UiNode {
        &mut self.view.nodes[self.index as usize]
    }

    pub fn text(mut self, text: impl Into<String>) -> Self {
        self.node().text = Some(text.into());
        self
    }

    pub fn attr(mut self, name: &str, value: impl Into<String>) -> Self {
        self.node().attrs.push((name.to_owned(), value.into()));
        self
    }

    pub fn class(self, class: &str) -> Self {
        self.attr("class", class)
    }

    /// Asks for an event: `click`, `input`, `change`, `submit`, `keydown`.
    pub fn on(mut self, event: &str) -> Self {
        self.node().on.push(event.to_owned());
        self
    }

    pub fn id(&self) -> u32 {
        self.index
    }
}

/// [`kv::get`] as text.
pub fn kv_text(key: &str) -> Option<String> {
    kv::get(key).and_then(|bytes| String::from_utf8(bytes).ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn views_are_flat_lists_naming_their_parents() {
        let mut v = View::new();
        let row = v.el(None, "div").class("app-row").id();
        v.el(Some(row), "span").text("0");
        let button = v.el(Some(row), "button").text("+1").on("click").id();
        let nodes = v.finish();
        assert_eq!(button, 2);
        assert_eq!(nodes.len(), 3);
        assert_eq!(nodes[0].attrs, [("class".to_owned(), "app-row".to_owned())]);
        assert_eq!(nodes[1].parent, Some(0));
        assert_eq!(nodes[2].on, ["click"]);
    }
}
