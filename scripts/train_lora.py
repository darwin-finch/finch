#!/usr/bin/env python3
"""
Experimental LoRA fine-tuning helper for Qwen-family base models

Reads weighted training examples from JSONL queue, trains LoRA adapters,
and exports PEFT weights in safetensors format. Finch does not currently load
this output into its llama.cpp chat runtime.

Usage:
    python3 scripts/train_lora.py <queue_jsonl> <output_adapter> --base-model <model_path>

Example:
    python3 scripts/train_lora.py \
        ~/.shammah/training_queue.jsonl \
        ~/.shammah/adapters/latest.safetensors \
        --base-model Qwen/Qwen2.5-1.5B-Instruct
"""

import argparse
import json
import sys
from pathlib import Path
from typing import List, Dict, Any

try:
    import torch
    from torch.utils.data import Dataset, DataLoader
    from transformers import (
        AutoModelForCausalLM,
        AutoTokenizer,
        TrainingArguments,
        Trainer,
        DataCollatorForLanguageModeling,
    )
    from peft import (
        LoraConfig,
        get_peft_model,
        TaskType,
        PeftModel,
    )
    from safetensors.torch import save_file
except ImportError as e:
    print(f"Error: Missing required package: {e}", file=sys.stderr)
    print("\nPlease install dependencies:", file=sys.stderr)
    print("  pip install torch transformers peft safetensors accelerate", file=sys.stderr)
    sys.exit(1)


class WeightedExampleDataset(Dataset):
    """Dataset that samples examples proportional to their weight"""

    def __init__(self, examples: List[Dict[str, Any]], tokenizer, max_length: int = 512):
        self.examples = examples
        self.tokenizer = tokenizer
        self.max_length = max_length

        # Build sampling weights (weight field in each example)
        self.weights = [ex.get("weight", 1.0) for ex in examples]
        self.total_weight = sum(self.weights)

        # Tokenize all examples upfront
        self.tokenized_examples = []
        for ex in examples:
            # Format as chat (system + user → assistant)
            chat = [
                {"role": "system", "content": "You are Qwen, a helpful AI assistant."},
                {"role": "user", "content": ex["query"]},
                {"role": "assistant", "content": ex["response"]},
            ]

            # Apply chat template
            text = self.tokenizer.apply_chat_template(
                chat,
                tokenize=False,
                add_generation_prompt=False,
            )

            # Tokenize
            encoded = self.tokenizer(
                text,
                truncation=True,
                max_length=self.max_length,
                padding="max_length",
                return_tensors="pt",
            )

            self.tokenized_examples.append({
                "input_ids": encoded["input_ids"].squeeze(0),
                "attention_mask": encoded["attention_mask"].squeeze(0),
                "labels": encoded["input_ids"].squeeze(0).clone(),
            })

    def __len__(self):
        return len(self.examples)

    def __getitem__(self, idx):
        return self.tokenized_examples[idx]

    def get_weighted_sampler(self):
        """Return a weighted sampler for DataLoader"""
        from torch.utils.data import WeightedRandomSampler
        return WeightedRandomSampler(
            weights=self.weights,
            num_samples=len(self.examples),
            replacement=True,
        )


def load_training_examples(jsonl_path: Path) -> List[Dict[str, Any]]:
    """Load weighted examples from JSONL file"""
    examples = []
    with open(jsonl_path, "r") as f:
        for line_num, line in enumerate(f, 1):
            try:
                example = json.loads(line)
                # Validate required fields
                if "query" not in example or "response" not in example:
                    print(f"Warning: Line {line_num} missing query/response, skipping", file=sys.stderr)
                    continue
                examples.append(example)
            except json.JSONDecodeError as e:
                print(f"Warning: Line {line_num} invalid JSON: {e}, skipping", file=sys.stderr)
                continue

    print(f"Loaded {len(examples)} training examples")

    # Print weight distribution
    weights = [ex.get("weight", 1.0) for ex in examples]
    print(f"Weight distribution:")
    print(f"  Min: {min(weights):.1f}")
    print(f"  Max: {max(weights):.1f}")
    print(f"  Mean: {sum(weights) / len(weights):.1f}")
    print(f"  High-weight (≥10): {sum(1 for w in weights if w >= 10)}")
    print(f"  Medium-weight (3-9): {sum(1 for w in weights if 3 <= w < 10)}")
    print(f"  Normal-weight (1-2): {sum(1 for w in weights if w < 3)}")

    return examples


def train_lora(
    model,
    tokenizer,
    dataset: WeightedExampleDataset,
    output_dir: Path,
    epochs: int = 3,
    batch_size: int = 4,
    learning_rate: float = 1e-4,
) -> PeftModel:
    """Train LoRA adapter on weighted examples"""

    print(f"\nTraining Configuration:")
    print(f"  Epochs: {epochs}")
    print(f"  Batch size: {batch_size}")
    print(f"  Learning rate: {learning_rate}")
    print(f"  Examples: {len(dataset)}")
    print(f"  Total training steps: {(len(dataset) // batch_size) * epochs}")

    # Create weighted sampler
    sampler = dataset.get_weighted_sampler()

    # Training arguments
    training_args = TrainingArguments(
        output_dir=str(output_dir / "checkpoints"),
        num_train_epochs=epochs,
        per_device_train_batch_size=batch_size,
        gradient_accumulation_steps=1,
        learning_rate=learning_rate,
        warmup_steps=10,
        logging_steps=5,
        save_steps=50,
        save_total_limit=2,
        fp16=torch.cuda.is_available(),  # Use FP16 on GPU
        report_to="none",  # Don't report to wandb/tensorboard
        remove_unused_columns=False,
    )

    # Data collator (handles batching)
    data_collator = DataCollatorForLanguageModeling(
        tokenizer=tokenizer,
        mlm=False,  # We're doing causal LM, not masked LM
    )

    # Create trainer
    trainer = Trainer(
        model=model,
        args=training_args,
        train_dataset=dataset,
        data_collator=data_collator,
    )

    # Train
    print("\nStarting training...")
    trainer.train()

    print("✅ Training complete!")
    return model


def export_adapter(model: PeftModel, output_path: Path):
    """Export LoRA adapter weights to safetensors"""
    print(f"\nExporting adapter to {output_path}")

    # Get adapter state dict (only LoRA parameters)
    adapter_state_dict = {}
    for name, param in model.named_parameters():
        if "lora" in name.lower():
            adapter_state_dict[name] = param.detach().cpu()

    print(f"Adapter contains {len(adapter_state_dict)} parameters")

    # Calculate total size
    total_params = sum(p.numel() for p in adapter_state_dict.values())
    total_size_mb = sum(p.numel() * p.element_size() for p in adapter_state_dict.values()) / (1024 * 1024)
    print(f"Total parameters: {total_params:,} ({total_size_mb:.2f} MB)")

    # Save to safetensors
    output_path.parent.mkdir(parents=True, exist_ok=True)
    save_file(adapter_state_dict, str(output_path))

    print(f"✅ Adapter saved successfully!")


def main():
    parser = argparse.ArgumentParser(description="Train LoRA adapter for Qwen models")
    parser.add_argument("queue_jsonl", type=Path, help="Path to training queue JSONL file")
    parser.add_argument("output_adapter", type=Path, help="Path to output safetensors file")
    parser.add_argument("--base-model", type=str, required=True, help="Base model name or path")
    parser.add_argument("--rank", type=int, default=16, help="LoRA rank (default: 16)")
    parser.add_argument("--alpha", type=float, default=32.0, help="LoRA alpha (default: 32.0)")
    parser.add_argument("--dropout", type=float, default=0.05, help="LoRA dropout (default: 0.05)")
    parser.add_argument("--epochs", type=int, default=3, help="Training epochs (default: 3)")
    parser.add_argument("--batch-size", type=int, default=4, help="Batch size (default: 4)")
    parser.add_argument("--learning-rate", type=float, default=1e-4, help="Learning rate (default: 1e-4)")

    args = parser.parse_args()

    # Validate inputs
    if not args.queue_jsonl.exists():
        print(f"Error: Training queue not found: {args.queue_jsonl}", file=sys.stderr)
        sys.exit(1)

    print("=" * 60)
    print("LoRA Fine-Tuning for Qwen Models")
    print("=" * 60)

    # Load examples
    print(f"\n📖 Loading training examples from {args.queue_jsonl}")
    examples = load_training_examples(args.queue_jsonl)

    if len(examples) == 0:
        print("Error: No valid examples found in training queue", file=sys.stderr)
        sys.exit(1)

    # Load tokenizer
    print(f"\n🔧 Loading tokenizer from {args.base_model}")
    tokenizer = AutoTokenizer.from_pretrained(args.base_model)

    # Load base model
    print(f"\n🤖 Loading base model from {args.base_model}")
    model = AutoModelForCausalLM.from_pretrained(
        args.base_model,
        torch_dtype=torch.float16 if torch.cuda.is_available() else torch.float32,
        device_map="auto" if torch.cuda.is_available() else None,
    )

    # Configure LoRA
    print(f"\n⚙️  Configuring LoRA")
    print(f"  Rank: {args.rank}")
    print(f"  Alpha: {args.alpha}")
    print(f"  Dropout: {args.dropout}")

    lora_config = LoraConfig(
        r=args.rank,
        lora_alpha=args.alpha,
        lora_dropout=args.dropout,
        target_modules=["q_proj", "v_proj", "k_proj", "o_proj"],  # Attention projections
        task_type=TaskType.CAUSAL_LM,
        bias="none",
    )

    # Apply LoRA
    model = get_peft_model(model, lora_config)
    model.print_trainable_parameters()

    # Create dataset
    print(f"\n📚 Creating weighted dataset")
    dataset = WeightedExampleDataset(examples, tokenizer)

    # Train
    trained_model = train_lora(
        model,
        tokenizer,
        dataset,
        args.output_adapter.parent,
        epochs=args.epochs,
        batch_size=args.batch_size,
        learning_rate=args.learning_rate,
    )

    # Export adapter
    export_adapter(trained_model, args.output_adapter)

    print("\n" + "=" * 60)
    print("✅ LoRA training complete!")
    print("=" * 60)
    print(f"\nAdapter saved to: {args.output_adapter}")
    print(f"You can now load this adapter in Rust runtime.")


if __name__ == "__main__":
    main()
