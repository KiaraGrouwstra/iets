//! Phase B: the remote builder client and the reference server.

pub mod client;
pub mod plan;
pub mod server;
pub mod worker;

#[allow(clippy::all, clippy::pedantic)]
pub mod proto {
    tonic::include_proto!("ietsp.builder.v2");
}
