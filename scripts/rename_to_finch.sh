#!/bin/bash
# Script to rename project from "shammah" to "finch"

set -e

echo "🐦 Renaming Shammah → Finch..."

# 1. Rename Cargo package
sed -i '' 's/name = "shammah"/name = "finch"/' Cargo.toml

# 2. Rename binary
sed -i '' 's/name = "shammah"/name = "finch"/' Cargo.toml

# 3. Find and replace in all source files
echo "Updating source files..."
find src -type f -name "*.rs" -exec sed -i '' 's/shammah/finch/g' {} +

# 4. Update documentation
echo "Updating documentation..."
find docs -type f -name "*.md" -exec sed -i '' 's/shammah/finch/g' {} +
find . -maxdepth 1 -type f -name "*.md" -exec sed -i '' 's/shammah/finch/g' {} +

# 5. Update TOML files
echo "Updating config files..."
find . -type f -name "*.toml" -exec sed -i '' 's/shammah/finch/g' {} +

# 6. Config directory migration will happen at runtime
echo "✅ Rename complete!"
echo ""
echo "Next steps:"
echo "1. Review changes: git diff"
echo "2. Test compilation: cargo build"
echo "3. Commit: git commit -am 'Rename project: Shammah → Finch'"
echo ""
echo "Config migration (~/.shammah → ~/.finch) will happen automatically on first run"
