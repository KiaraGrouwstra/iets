//! Phase B: the remote builder client and the reference server.

pub mod chunk;
pub mod client;
pub mod plan;
pub mod server;

#[allow(clippy::all, clippy::pedantic)]
pub mod proto {
    tonic::include_proto!("ietsp.builder.v1");
}
