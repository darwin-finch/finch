fn main() {
    println!("cargo:rerun-if-changed=schema/finch_ipc.capnp");
    capnpc::CompilerCommand::new()
        .src_prefix("schema")
        .file("schema/finch_ipc.capnp")
        .run()
        .expect("capnp schema compilation failed — is `capnp` installed? (brew install capnp / apt install capnproto)");
}
