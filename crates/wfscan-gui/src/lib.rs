//! wfscan-gui: a desktop front end for wfscan-core, built with egui. It calls the library
//! directly and never shells out to the CLI.
//!
//! [`session`] holds the state and the slow operations, with no drawing code, so it can
//! be tested headlessly; [`jobs`] runs those operations off the UI thread; [`preview`]
//! loads the selected file on its own thread; [`player`] plays a decoded sound; [`tree`] arranges the
//! files by folder; [`app`] draws.

pub mod app;
pub mod jobs;
pub mod player;
pub mod preview;
pub mod session;
pub mod settings;
pub mod tree;
pub mod widgets;

pub use app::App;
