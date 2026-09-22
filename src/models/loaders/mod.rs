// Model loaders: ONNX Runtime (default) and Candle (optional)
#[cfg(feature = "llama-cpp")]
pub mod llama_cpp;
pub mod onnx;
pub mod onnx_config;

#[cfg(feature = "candle")]
pub mod candle;
