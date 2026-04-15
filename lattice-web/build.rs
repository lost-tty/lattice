fn main() {
    // No build-time codegen — the HTTP/SSE handlers route raw protobuf
    // through tonic without a wrapping wire format.
}
