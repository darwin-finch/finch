// Model loaders: ONNX Runtime (default) and Candle (optional)
pub mod onnx;
pub mod onnx_config;

#[cfg(feature = "candle")]
pub mod candle;
