mod loader;
mod umem;
mod socket;
mod receiver;

pub use loader::XdpHandle;
pub use receiver::{start_xdp_receive_path, run_xdp_sender_worker, iface_for_benchmark};
