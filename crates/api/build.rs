//! Generates the etcd wire types from the vendored `.proto` files.
//!
//! Compilation goes through `protox` rather than `protoc`, so a plain
//! `cargo build` needs no protobuf compiler installed out of band.

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut fds = protox::compile(["etcd/api/etcdserverpb/rpc.proto"], ["../../proto"])?;

    // The gogo, googleapis, and grpc-gateway protos are vendored only so
    // etcd's imports and option annotations resolve. Generating Rust for them
    // adds ~1,400 lines of types nothing references, and pulls in a
    // `prost-types` dependency for the openapiv2 module alone. Each retained
    // file's dependency list is filtered too, or prost-build fails on the
    // entries pointing at files that are no longer present.

    fds.file.retain(|f| f.name().starts_with("etcd/api/"));
    for file in &mut fds.file {
        file.dependency.retain(|d| d.starts_with("etcd/api/"));
    }

    tonic_prost_build::configure()
        .build_client(false)
        .build_server(true)
        .include_file("mod.rs")
        .compile_fds(fds)?;

    println!("cargo:rerun-if-changed=../../proto");
    Ok(())
}
