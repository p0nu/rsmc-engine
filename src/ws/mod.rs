//! WebSocket subsystem: the connection [`handler`] and the in-process [`hub`]
//! that tracks live sessions and fans events out to them.

pub mod handler;
pub mod hub;

pub use handler::ws_handler;
pub use hub::Hub;
