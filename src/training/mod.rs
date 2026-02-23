// Training module - Batch training and checkpoint management

pub mod batch_trainer;
pub mod checkpoint;
pub mod lora_subprocess; // Phase 6: Python LoRA training subprocess

pub use batch_trainer::{BatchTrainer, TrainingExample, TrainingResult};
pub use checkpoint::{Checkpoint, CheckpointManager};
pub use lora_subprocess::{LoRATrainingConfig, LoRATrainingSubprocess};
