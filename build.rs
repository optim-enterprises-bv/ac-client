fn main() {
    // Compile the shared ACP protobuf schema.
    // Proto files are vendored in proto/ (previously shared with ac-server).
    prost_build::compile_protos(
        &["proto/acp.proto"],
        &["proto/"],
    )
    .expect("prost-build: failed to compile acp.proto");

    // TR-369 / USP wire protocol.
    prost_build::compile_protos(
        &["proto/usp-record.proto", "proto/usp-msg.proto"],
        &["proto/"],
    )
    .expect("prost_build: failed to compile USP protos");
}
