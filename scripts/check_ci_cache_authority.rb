#!/usr/bin/env ruby
# frozen_string_literal: true

require "psych"

WORKFLOW_PATH = ".github/workflows/ci.yml"
CACHE_SHA = "0057852bfaa89a56745cba8c7296529d2fc39830"
RESTORE_ACTION = "actions/cache/restore@#{CACHE_SHA}"
SAVE_ACTION = "actions/cache/save@#{CACHE_SHA}"
RESTORE_ID = "cargo-cache-restore"
CACHE_PATHS = ["~/.cargo/registry", "~/.cargo/git", "target"].freeze
CACHE_KEY = "${{ runner.os }}-rust-1.98.0-${{ matrix.feature_name }}-${{ hashFiles('**/Cargo.lock') }}"
RESTORE_PREFIX = "${{ runner.os }}-rust-1.98.0-${{ matrix.feature_name }}-"
SAVE_KEY = "${{ steps.#{RESTORE_ID}.outputs.cache-primary-key }}"
SAVE_CONDITION = "${{ success() && github.event_name == 'push' && github.ref == 'refs/heads/main' && steps.#{RESTORE_ID}.outputs.cache-hit != 'true' }}"

EXPECTED_MATRIX = [
  { "os" => "ubuntu-24.04", "feature_name" => "default", "cargo_args" => "" },
  { "os" => "ubuntu-24.04", "feature_name" => "no-default-features", "cargo_args" => "--no-default-features" },
  { "os" => "macos-14", "feature_name" => "default", "cargo_args" => "" },
  { "os" => "macos-14", "feature_name" => "no-default-features", "cargo_args" => "--no-default-features" }
].freeze

EXPECTED_STEP_NAMES = [
  "Checkout code",
  "Install capnproto (Ubuntu)",
  "Install capnproto (macOS)",
  "Install repository Rust toolchain",
  "Restore Cargo registry and build cache",
  "Run clippy (binary only, warnings allowed for now)",
  "Build binary",
  "Compile all targets",
  "Test all targets",
  "Verify binary reports its version",
  "Save Cargo registry and build cache"
].freeze

EXPECTED_WORKLOADS = {
  "Run clippy (binary only, warnings allowed for now)" => {
    "if" => "runner.os == 'Linux' && matrix.feature_name == 'default'",
    "run" => "cargo clippy --bin finch --all-features",
    "continue-on-error" => true
  },
  "Build binary" => {
    "run" => "cargo build --bin finch ${{ matrix.cargo_args }} --verbose"
  },
  "Compile all targets" => {
    "run" => "cargo test --all-targets ${{ matrix.cargo_args }} --no-run"
  },
  "Test all targets" => {
    "run" => "cargo test --all-targets ${{ matrix.cargo_args }} -- --nocapture"
  },
  "Verify binary reports its version" => {
    "run" => "cargo run ${{ matrix.cargo_args }} -- --version"
  }
}.freeze

EXPECTED_ACTIONS = [
  "actions/checkout@v4",
  "dtolnay/rust-toolchain@1.98.0",
  RESTORE_ACTION,
  SAVE_ACTION
].freeze

def multiline_lines(value)
  value.to_s.lines.map(&:strip).reject(&:empty?)
end

def add_mismatch(errors, label, actual, expected)
  return if actual == expected

  errors << "#{label} drifted\n  expected: #{expected.inspect}\n  actual:   #{actual.inspect}"
end

def cache_contract_errors(job)
  errors = []
  unless job.is_a?(Hash)
    return ["jobs.test must be a mapping; found #{job.inspect}"]
  end

  add_mismatch(errors, "jobs.test runs-on", job["runs-on"], "${{ matrix.os }}")
  errors << "jobs.test must not have a job-level if condition that can disable cache/workload execution" if job.key?("if")
  errors << "jobs.test must not define defaults that can reinterpret the bounded workload commands" if job.key?("defaults")
  errors << "jobs.test must not define job-level continue-on-error" if job.key?("continue-on-error")

  strategy = job["strategy"]
  if strategy.is_a?(Hash)
    add_mismatch(errors, "jobs.test strategy keys", strategy.keys, ["fail-fast", "matrix"])
    add_mismatch(errors, "jobs.test fail-fast", strategy["fail-fast"], false)
    matrix = strategy["matrix"]
    if matrix.is_a?(Hash)
      add_mismatch(errors, "jobs.test matrix dimensions", matrix.keys, ["include"])
      add_mismatch(errors, "jobs.test matrix include (exact four OS/feature rows)", matrix["include"], EXPECTED_MATRIX)
    else
      errors << "jobs.test strategy.matrix must be a mapping; found #{matrix.inspect}"
    end
  else
    errors << "jobs.test strategy must be a mapping; found #{strategy.inspect}"
  end

  steps = job["steps"]
  unless steps.is_a?(Array) && steps.all? { |step| step.is_a?(Hash) }
    errors << "jobs.test steps must be an array of mappings; found #{steps.inspect}"
    return errors
  end

  add_mismatch(errors, "jobs.test step names/order", steps.map { |step| step["name"] }, EXPECTED_STEP_NAMES)
  action_uses = steps.map { |step| step["uses"] }.compact
  add_mismatch(errors, "jobs.test action identities/order (no alternate or additional cache action)", action_uses, EXPECTED_ACTIONS)

  named_steps = steps.group_by { |step| step["name"] }
  EXPECTED_WORKLOADS.each do |name, expected|
    matches = named_steps.fetch(name, [])
    if matches.length != 1
      errors << "workload step #{name.inspect} must occur exactly once; found #{matches.length}"
      next
    end

    actual = matches.first
    add_mismatch(errors, "workload #{name.inspect} executable fields", actual.reject { |key, _| key == "name" }, expected)
  end

  restore_steps = steps.select { |step| step["uses"] == RESTORE_ACTION }
  save_steps = steps.select { |step| step["uses"] == SAVE_ACTION }
  errors << "expected exactly one pinned cache restore; found #{restore_steps.length}" unless restore_steps.length == 1
  errors << "expected exactly one pinned cache save; found #{save_steps.length}" unless save_steps.length == 1
  return errors unless restore_steps.length == 1 && save_steps.length == 1

  restore = restore_steps.first
  save = save_steps.first
  restore_index = steps.index(restore)
  save_index = steps.index(save)
  workload_indices = EXPECTED_WORKLOADS.keys.map do |name|
    index = steps.index { |step| step["name"] == name }
    index unless index.nil?
  end.compact

  errors << "cache restore must have stable id #{RESTORE_ID.inspect}; found #{restore['id'].inspect}" unless restore["id"] == RESTORE_ID
  errors << "cache restore must be unconditional; found if: #{restore['if'].inspect}" if restore.key?("if")
  add_mismatch(errors, "cache restore enabled/nonfatal fields", restore.keys, ["name", "id", "uses", "continue-on-error", "with"])
  add_mismatch(errors, "cache restore continue-on-error", restore["continue-on-error"], true)
  add_mismatch(errors, "cache restore inputs", restore.fetch("with", {}).keys, ["path", "key", "restore-keys"])
  add_mismatch(errors, "cache restore paths", multiline_lines(restore.dig("with", "path")), CACHE_PATHS)
  add_mismatch(errors, "cache restore primary key", restore.dig("with", "key"), CACHE_KEY)
  add_mismatch(errors, "cache restore prefix", multiline_lines(restore.dig("with", "restore-keys")), [RESTORE_PREFIX])

  add_mismatch(errors, "cache save enabled/nonfatal fields", save.keys, ["name", "if", "uses", "continue-on-error", "with"])
  add_mismatch(errors, "cache save continue-on-error", save["continue-on-error"], true)
  add_mismatch(errors, "cache save trusted successful-main miss-only condition", save["if"], SAVE_CONDITION)
  add_mismatch(errors, "cache save inputs", save.fetch("with", {}).keys, ["path", "key"])
  add_mismatch(errors, "cache save paths", multiline_lines(save.dig("with", "path")), CACHE_PATHS)
  add_mismatch(errors, "cache save primary-key handoff", save.dig("with", "key"), SAVE_KEY)

  unless workload_indices.empty? || restore_index < workload_indices.min
    errors << "cache restore step index #{restore_index} must precede every workload at indices #{workload_indices.inspect}"
  end
  unless workload_indices.empty? || save_index > workload_indices.max
    errors << "cache save step index #{save_index} must follow every workload at indices #{workload_indices.inspect}"
  end
  errors << "cache save must be the final jobs.test step; save index #{save_index}, final index #{steps.length - 1}" unless save_index == steps.length - 1

  errors
end

def wiring_errors(path = "tests/toolchain_contract.sh")
  expected = [
    "ruby tests/test_ci_cache_authority.rb",
    "ruby scripts/check_ci_cache_authority.rb"
  ]
  lines = File.readlines(path, chomp: true)
  expected.map do |invocation|
    count = lines.count { |line| line == invocation }
    next if count == 1

    "#{path} must invoke #{invocation.inspect} exactly once; found #{count}"
  end.compact
end

workflow_path = ARGV.fetch(0, WORKFLOW_PATH)
unless ARGV.length <= 1
  warn "usage: #{$PROGRAM_NAME} [ci-workflow-path]"
  exit 2
end

begin
  document = Psych.safe_load(File.read(workflow_path), aliases: false)
rescue Errno::ENOENT, Psych::Exception => e
  warn "cannot parse #{workflow_path}: #{e.class}: #{e.message}"
  exit 1
end

job = document.is_a?(Hash) ? document.dig("jobs", "test") : nil
errors = cache_contract_errors(job)
errors.concat(wiring_errors) if File.expand_path(workflow_path) == File.expand_path(WORKFLOW_PATH)

if errors.any?
  warn "#{workflow_path} violates the canonical jobs.test cache-authority contract:"
  errors.each { |error| warn "- #{error}" }
  warn "parsed jobs.test state:"
  warn Psych.dump(job)
  exit 1
end

puts "#{workflow_path}: canonical jobs.test cache authority is valid"
