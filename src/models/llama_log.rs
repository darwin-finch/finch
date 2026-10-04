//! Routing llama.cpp's native log output into Finch's own diagnostics.
//!
//! llama.cpp and ggml write to the process's stderr by default. In the
//! interactive frontend that bypasses the renderer: the text lands on top of
//! the TUI while a model loads and is gone on the next repaint. Every backend
//! initialisation installs this hook first, so the same lines become
//! `tracing` events instead. Those reach the frontend's diagnostic log, which
//! is what the Ctrl+` console shows.

/// Send llama.cpp and ggml log output to `tracing` for the rest of the
/// process. Call before `LlamaBackend::init`, which already logs (Metal
/// device discovery, for example).
///
/// Do not follow this with `LlamaBackend::void_logs`: that installs a
/// different callback which discards everything, including the model-load
/// output this hook exists to keep.
pub(crate) fn route_native_logs_to_tracing() {
    llama_cpp_2::send_logs_to_tracing(llama_cpp_2::LogOptions::default().with_logs_enabled(true));
}

#[cfg(test)]
mod tests {
    /// The reported failure: llama.cpp's load output flashed over the TUI and
    /// never appeared in the Ctrl+` console. Neither backend initialisation
    /// may let native output reach stderr or throw it away: each installs the
    /// hook before `LlamaBackend::init` and never calls `void_logs`, which
    /// would replace the hook with one that discards the model-load output.
    #[test]
    fn test_every_llama_backend_init_routes_logs_and_never_voids_them() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/models");
        for file in ["loaders/llama_cpp.rs", "neural_embedding.rs"] {
            let source = std::fs::read_to_string(root.join(file)).expect("read backend source");
            let hook = source.find("route_native_logs_to_tracing()");
            let init = source.find("LlamaBackend::init()");
            assert!(
                matches!((hook, init), (Some(hook), Some(init)) if hook < init),
                "{file}: the log hook must be installed before LlamaBackend::init, or \
                 start-up output goes to stderr over the TUI; hook={hook:?} init={init:?}"
            );
            assert!(
                !source.contains(".void_logs()"),
                "{file}: void_logs replaces the hook and discards the load output that \
                 belongs in the diagnostic console"
            );
        }
        let hook = include_str!("llama_log.rs");
        assert!(
            hook.contains("llama_cpp_2::send_logs_to_tracing(")
                && hook.contains("with_logs_enabled(true)"),
            "the hook must send llama.cpp and ggml output to tracing, not suppress it"
        );
    }
}
