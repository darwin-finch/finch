#!/usr/bin/env ruby
# frozen_string_literal: true

# Semantic production-boundary contract for Finch's canonical Cargo caches.
# It parses the checked-in YAML so comments and inactive text cannot satisfy a
# requirement, then validates authority, ordering, paths, pins, and matrices.

require "shellwords"
require "yaml"

ROOT = File.expand_path("..", __dir__)
CACHE_SHA = "0057852bfaa89a56745cba8c7296529d2fc39830"
SCCACHE_SHA = "fc920bf0ec8de6ee65d409111f7ec508035751ba"
CACHE_RESTORE = "actions/cache/restore@#{CACHE_SHA}"
CACHE_SAVE = "actions/cache/save@#{CACHE_SHA}"
SCCACHE_ACTION = "mozilla-actions/sccache-action@#{SCCACHE_SHA}"
TRUSTED_MAIN = "github.event_name == 'push' && github.ref == 'refs/heads/main'"
DOWNLOAD_PATHS = [
  "${{ runner.temp }}/finch-cargo-home/git/db",
  "${{ runner.temp }}/finch-cargo-home/registry/cache"
].freeze
AUDIT_PATHS = ["${{ runner.temp }}/finch-cargo-home/bin/cargo-audit"].freeze
KNOWN_CONSUMERS = {
  ".github/workflows/ci.yml" => %w[test runtime-authority build security],
  ".github/workflows/release.yml" => %w[build-release]
}.freeze
SCCACHE_CONSUMERS = {
  ".github/workflows/ci.yml" => %w[test runtime-authority build],
  ".github/workflows/release.yml" => %w[build-release]
}.freeze
COMPILE_SUBCOMMANDS = %w[bench build check clippy doc install run rustc test].freeze

def disabled?(item)
  condition = item["if"]
  return false if condition.nil?
  return true if condition == false

  normalized = condition.to_s.gsub(/\s+/, " ").strip
  normalized == "false" || normalized == "${{ false }}"
end

def steps(job)
  value = job["steps"]
  value.is_a?(Array) ? value.select { |step| step.is_a?(Hash) } : []
end

def step_label(step)
  step["name"] || step["uses"] || step["run"]&.lines&.first&.strip || "unnamed step"
end

def active_steps(job)
  steps(job).reject { |step| disabled?(step) }
end

def paths(step)
  step.dig("with", "path").to_s.lines.map(&:strip).reject(&:empty?).sort
end

def exact_download_key?(key)
  text = key.to_s
  text.start_with?("cargo-downloads-v1-${{ runner.os }}-rust-1.98.0-config-") &&
    text.include?("hashFiles('rust-toolchain.toml', '.cargo/config', '.cargo/config.toml')") &&
    text.end_with?("-lock-${{ hashFiles('**/Cargo.lock') }}")
end

def cache_action?(step)
  step["uses"].to_s.downcase.include?("cache")
end

def split_shell(script)
  script.to_s.lines.flat_map { |line| line.split(/\s*(?:&&|\|\||;|\|)\s*/) }
end

def resolved_executable(token, variables)
  match = token.match(/\A\$\{?([A-Za-z_][A-Za-z0-9_]*)\}?\z/)
  return variables[match[1]] if match

  token
end

def cargo_compile_script?(script)
  variables = {}
  split_shell(script).any? do |statement|
    stripped = statement.strip
    next false if stripped.empty? || stripped.start_with?("#")

    tokens = Shellwords.shellsplit(stripped)
    while tokens.first&.match?(/\A[A-Za-z_][A-Za-z0-9_]*=.*/)
      name, value = tokens.shift.split("=", 2)
      variables[name] = value
    end
    next false if tokens.empty?

    executable = resolved_executable(tokens.first, variables)
    if executable == "echo" || executable == "printf"
      next false
    end

    cargo_at = if executable == "cargo"
                 0
               elsif %w[command env sudo timeout].include?(executable) || executable.end_with?("test_brains.sh")
                 tokens.index("cargo")
               elsif executable.start_with?("$")
                 0
               end
    next false if cargo_at.nil?

    tokens[(cargo_at + 1)..].to_a.any? { |token| COMPILE_SUBCOMMANDS.include?(token) }
  rescue ArgumentError
    # A malformed/opaque shell command is not silently accepted when it looks
    # like it might invoke a compiling Cargo subcommand indirectly.
    stripped.match?(/\bcargo\b.*\b(?:#{COMPILE_SUBCOMMANDS.join('|')})\b/)
  end
end

def first_compile_index(job)
  steps(job).each_with_index do |step, index|
    next if disabled?(step)
    return index if cargo_compile_script?(step["run"])
  end
  nil
end

def find_steps(job, &block)
  steps(job).each_with_index.select { |step, _index| block.call(step) }
end

def actionable(errors, workflow, job, message)
  errors << "#{workflow} job '#{job}': #{message}"
end

def validate_cache_action(step, errors, workflow, job)
  uses = step["uses"].to_s
  return unless cache_action?(step)
  if [CACHE_RESTORE, CACHE_SAVE, SCCACHE_ACTION].include?(uses)
    if uses == CACHE_RESTORE && step.fetch("with", {}).key?("restore-keys")
      actionable(errors, workflow, job, "exact cache restore must not have cumulative restore-keys")
    end
    return
  end

  actionable(errors, workflow, job,
             "cache-like action #{uses.inspect} is not an approved full-SHA-pinned cache action")
end

def validate_download_restore(step, errors, workflow, job)
  actionable(errors, workflow, job, "dependency restore must use #{CACHE_RESTORE}") unless step["uses"] == CACHE_RESTORE
  actionable(errors, workflow, job, "dependency restore must be nonfatal") unless step["continue-on-error"] == true
  actionable(errors, workflow, job,
             "dependency restore paths must be exactly #{DOWNLOAD_PATHS.inspect}; found #{paths(step).inspect}") unless paths(step) == DOWNLOAD_PATHS
  actionable(errors, workflow, job, "dependency restore key is missing an exact compatible lock identity") unless exact_download_key?(step.dig("with", "key"))
  if step.fetch("with", {}).key?("restore-keys")
    actionable(errors, workflow, job, "exact dependency restore must not have cumulative restore-keys")
  end
  actionable(errors, workflow, job, "dependency restore is disabled") if disabled?(step)
  actionable(errors, workflow, job, "dependency restore must be unconditional") unless step["if"].nil?
end

def validate_sccache(job_value, errors, workflow, job_name, writer)
  setup = find_steps(job_value) { |step| step["uses"].to_s.include?("sccache-action") }
  if setup.length != 1
    actionable(errors, workflow, job_name, "needs exactly one pinned sccache setup; found #{setup.length}")
    return
  end
  setup_step, setup_at = setup.first
  actionable(errors, workflow, job_name, "sccache setup must use #{SCCACHE_ACTION}") unless setup_step["uses"] == SCCACHE_ACTION
  actionable(errors, workflow, job_name, "sccache setup must pin binary v0.17.0") unless setup_step.dig("with", "version") == "v0.17.0"
  actionable(errors, workflow, job_name, "sccache setup must be nonfatal") unless setup_step["continue-on-error"] == true
  actionable(errors, workflow, job_name, "sccache setup is disabled") if disabled?(setup_step)
  actionable(errors, workflow, job_name, "sccache setup must be unconditional") unless setup_step["if"].nil?

  configure = find_steps(job_value) { |step| step["run"].to_s.strip == "scripts/configure_ci_sccache.sh" }
  if configure.length != 1
    actionable(errors, workflow, job_name, "needs exactly one compiler-cache authority configuration; found #{configure.length}")
    return
  end
  configure_step, configure_at = configure.first
  actual_writer = configure_step.dig("env", "FINCH_SCCACHE_WRITER")
  actionable(errors, workflow, job_name,
             "compiler-cache writer marker must be #{writer.inspect}; found #{actual_writer.inspect}") unless actual_writer == writer
  actionable(errors, workflow, job_name, "compiler-cache authority configuration is disabled") if disabled?(configure_step)
  actionable(errors, workflow, job_name, "compiler-cache authority configuration must be unconditional") unless configure_step["if"].nil?

  compile_at = first_compile_index(job_value)
  return if compile_at.nil?
  unless setup_at < configure_at && configure_at < compile_at
    actionable(errors, workflow, job_name,
               "must install and authorize sccache before the first compiling Cargo command")
  end
end

def validate_consumer(job_value, errors, workflow, job_name)
  compile_at = first_compile_index(job_value)
  if compile_at.nil?
    actionable(errors, workflow, job_name, "is contracted as expensive but no active compiling Cargo command was found")
    return
  end

  cargo_home = find_steps(job_value) { |step| step["run"].to_s.strip == "scripts/configure_ci_cargo_home.sh" }
  lock = find_steps(job_value) { |step| step["run"].to_s.strip == "cargo generate-lockfile" }
  restores = find_steps(job_value) do |step|
    step["uses"].to_s.start_with?("actions/cache/restore@") &&
      step.dig("with", "key").to_s.start_with?("cargo-downloads-")
  end
  actionable(errors, workflow, job_name, "needs exactly one active isolated Cargo home setup; found #{cargo_home.length}") unless cargo_home.length == 1 && !disabled?(cargo_home.first.first)
  actionable(errors, workflow, job_name, "needs exactly one active lock resolution; found #{lock.length}") unless lock.length == 1 && !disabled?(lock.first.first)
  actionable(errors, workflow, job_name, "needs exactly one exact dependency restore; found #{restores.length}") unless restores.length == 1
  return unless cargo_home.length == 1 && lock.length == 1 && restores.length == 1

  actionable(errors, workflow, job_name, "isolated Cargo home setup must be unconditional") unless cargo_home.first.first["if"].nil?
  actionable(errors, workflow, job_name, "lock resolution must be unconditional") unless lock.first.first["if"].nil?
  validate_download_restore(restores.first.first, errors, workflow, job_name)
  unless cargo_home.first.last < lock.first.last && lock.first.last < restores.first.last && restores.first.last < compile_at
    actionable(errors, workflow, job_name,
               "must isolate Cargo home, resolve the lock, restore exact downloads, then run the first Cargo compile")
  end
end

def validate_matrix(errors, ci_jobs)
  producer_os = ci_jobs.dig("cargo-download-cache", "strategy", "matrix", "os")
  unless producer_os == %w[ubuntu-24.04 macos-14]
    actionable(errors, ".github/workflows/ci.yml", "cargo-download-cache",
               "active OS matrix must be [ubuntu-24.04, macos-14]; found #{producer_os.inspect}")
  end

  test_rows = ci_jobs.dig("test", "strategy", "matrix", "include")
  expected_test = [
    ["ubuntu-24.04", "default", "", true],
    ["ubuntu-24.04", "no-default-features", "--no-default-features", false],
    ["macos-14", "default", "", true],
    ["macos-14", "no-default-features", "--no-default-features", false]
  ]
  actual_test = Array(test_rows).map do |row|
    [row["os"], row["feature_name"], row["cargo_args"], row["sccache_writer"]]
  end
  actionable(errors, ".github/workflows/ci.yml", "test",
             "matrix must bind the two trusted writer lanes to default features; found #{actual_test.inspect}") unless actual_test == expected_test

  build_rows = ci_jobs.dig("build", "strategy", "matrix", "include")
  expected_build = [
    ["ubuntu-24.04", "x86_64-unknown-linux-gnu", true],
    ["macos-14", "aarch64-apple-darwin", true]
  ]
  actual_build = Array(build_rows).map { |row| [row["os"], row["target"], row["sccache_writer"]] }
  actionable(errors, ".github/workflows/ci.yml", "build",
             "matrix must bind one trusted release-profile writer per supported OS; found #{actual_build.inspect}") unless actual_build == expected_build
end

def validate_release_matrix(errors, release_jobs)
  rows = release_jobs.dig("build-release", "strategy", "matrix", "include")
  expected = [
    ["macos-14", "aarch64-apple-darwin", "finch-macos-arm64"],
    ["ubuntu-24.04", "x86_64-unknown-linux-gnu", "finch-linux-x86_64"]
  ]
  actual = Array(rows).map { |row| [row["os"], row["target"], row["asset_name"]] }
  actionable(errors, ".github/workflows/release.yml", "build-release",
             "active release matrix must bind each supported OS, target, and asset; found #{actual.inspect}") unless actual == expected
end

def validate_download_producer(job, errors)
  workflow = ".github/workflows/ci.yml"
  name = "cargo-download-cache"
  actionable(errors, workflow, name, "job must be active only on trusted main pushes") unless job["if"] == TRUSTED_MAIN

  cargo_home = find_steps(job) { |step| step["run"].to_s.strip == "scripts/configure_ci_cargo_home.sh" }
  lock = find_steps(job) { |step| step["run"].to_s.strip == "cargo generate-lockfile" }
  restores = find_steps(job) { |step| step["uses"] == CACHE_RESTORE }
  fetches = find_steps(job) do |step|
    run = step["run"].to_s
    run.strip == "cargo fetch --locked --verbose"
  end
  saves = find_steps(job) { |step| step["uses"] == CACHE_SAVE }
  actionable(errors, workflow, name, "needs one isolated Cargo home, lock resolution, exact restore, complete fetch, and save") unless [cargo_home.length, lock.length, restores.length, fetches.length, saves.length] == [1, 1, 1, 1, 1]
  return unless [cargo_home.length, lock.length, restores.length, fetches.length, saves.length] == [1, 1, 1, 1, 1]

  validate_download_restore(restores.first.first, errors, workflow, name)
  fetch_step, fetch_at = fetches.first
  save_step, save_at = saves.first
  actionable(errors, workflow, name, "clean complete fetch is disabled") if disabled?(fetch_step)
  unless fetch_step["if"] == "steps.cargo-downloads.outputs.cache-hit != 'true'"
    actionable(errors, workflow, name, "clean complete fetch must run on an exact-cache miss")
  end
  actionable(errors, workflow, name, "save must be nonfatal") unless save_step["continue-on-error"] == true
  expected_save = "steps.cargo-downloads.outputs.cache-hit != 'true' && #{TRUSTED_MAIN}"
  unless save_step["if"] == expected_save && !disabled?(save_step)
    actionable(errors, workflow, name, "save must be executable only after a trusted-main cache miss")
  end
  actionable(errors, workflow, name,
             "save paths must be exactly #{DOWNLOAD_PATHS.inspect}; found #{paths(save_step).inspect}") unless paths(save_step) == DOWNLOAD_PATHS
  unless save_step.dig("with", "key") == "${{ steps.cargo-downloads.outputs.cache-primary-key }}"
    actionable(errors, workflow, name, "save must reuse the exact restore primary key")
  end
  unless cargo_home.first.last < lock.first.last && lock.first.last < restores.first.last && restores.first.last < fetch_at && fetch_at < save_at
    actionable(errors, workflow, name, "must isolate Cargo home, resolve, restore, fetch, then save in that order")
  end
end

def validate_audit(job, errors)
  workflow = ".github/workflows/ci.yml"
  name = "security"
  restores = find_steps(job) { |step| step["uses"] == CACHE_RESTORE && paths(step) == AUDIT_PATHS }
  installs = find_steps(job) { |step| step["run"].to_s.strip == "cargo install cargo-audit --version 0.22.2 --locked" }
  saves = find_steps(job) { |step| step["uses"] == CACHE_SAVE }
  actionable(errors, workflow, name, "needs one pinned cargo-audit restore/install/save path") unless [restores.length, installs.length, saves.length] == [1, 1, 1]
  return unless [restores.length, installs.length, saves.length] == [1, 1, 1]

  restore, restore_at = restores.first
  install, install_at = installs.first
  save, save_at = saves.first
  actionable(errors, workflow, name, "cargo-audit restore must be nonfatal") unless restore["continue-on-error"] == true
  expected_key = "cargo-tool-v1-${{ runner.os }}-ubuntu-24.04-rust-1.98.0-cargo-audit-0.22.2"
  actionable(errors, workflow, name, "cargo-audit key must pin OS, runner, Rust, and tool version") unless restore.dig("with", "key") == expected_key
  actionable(errors, workflow, name, "cargo-audit install must run only on a cache miss") unless install["if"] == "steps.cargo-audit.outputs.cache-hit != 'true'"
  actionable(errors, workflow, name, "cargo-audit save must be nonfatal") unless save["continue-on-error"] == true
  expected_save = "steps.cargo-audit.outputs.cache-hit != 'true' && #{TRUSTED_MAIN}"
  unless save["if"] == expected_save && !disabled?(save)
    actionable(errors, workflow, name, "cargo-audit save must be executable only after a trusted-main miss")
  end
  actionable(errors, workflow, name,
             "cargo-audit save paths must be exactly #{AUDIT_PATHS.inspect}; found #{paths(save).inspect}") unless paths(save) == AUDIT_PATHS
  actionable(errors, workflow, name, "cargo-audit save must reuse the restore primary key") unless save.dig("with", "key") == "${{ steps.cargo-audit.outputs.cache-primary-key }}"
  unless restore_at < install_at && install_at < save_at
    actionable(errors, workflow, name, "must restore, install on miss, verify, then save")
  end
  full_job = steps(job).map { |step| step["run"].to_s }.join("\n")
  unless full_job.include?("actual=$(cargo-audit --version)") && full_job.include?('[[ "$actual" != "cargo-audit 0.22.2" ]]')
    actionable(errors, workflow, name, "must verify the direct cached cargo-audit executable is exactly 0.22.2")
  end
end

def validate_permissions(documents, errors)
  ci = documents.fetch(".github/workflows/ci.yml")
  unless ci["permissions"] == { "contents" => "read" }
    errors << ".github/workflows/ci.yml: workflow permissions must be exactly contents: read"
  end
  (ci["jobs"] || {}).each do |job_name, job|
    next unless job.fetch("permissions", {}).values.include?("write")
    actionable(errors, ".github/workflows/ci.yml", job_name, "must not elevate token permissions")
  end
  release = documents.fetch(".github/workflows/release.yml")
  unless release["permissions"] == { "contents" => "read" }
    errors << ".github/workflows/release.yml: workflow permissions must be exactly contents: read"
  end
  create = release.dig("jobs", "create-release") || {}
  unless create["permissions"] == { "contents" => "write" }
    actionable(errors, ".github/workflows/release.yml", "create-release",
               "must be the only job elevated to contents: write")
  end
  steps(create).each do |step|
    next unless step["uses"]
    ref = step["uses"].to_s.split("@").last
    actionable(errors, ".github/workflows/release.yml", "create-release",
               "action #{step['uses'].inspect} executes with release authority and must use a full commit SHA") unless ref&.match?(/\A[0-9a-f]{40}\z/)
  end
  (release["jobs"] || {}).each do |job_name, job|
    next if job_name == "create-release"
    next unless job.fetch("permissions", {}).values.include?("write")
    actionable(errors, ".github/workflows/release.yml", job_name,
               "must not receive release write authority")
  end
end

def validate_helpers(root, errors)
  sccache_path = File.join(root, "scripts/configure_ci_sccache.sh")
  cargo_home_path = File.join(root, "scripts/configure_ci_cargo_home.sh")
  unless File.executable?(sccache_path) && File.executable?(cargo_home_path)
    errors << "CI cache helpers must both exist and be executable"
    return
  end

  sccache = File.read(sccache_path)
  required_sccache = [
    '-z "${SCCACHE_PATH:-}" || ! -x "${SCCACHE_PATH}"',
    'cache_mode=READ_ONLY',
    '"${GITHUB_EVENT_NAME:-}" == "push"',
    '"${GITHUB_REF:-}" == "refs/heads/main"',
    '"${FINCH_SCCACHE_WRITER:-false}" == "true"',
    'echo "CARGO_INCREMENTAL=0"',
    'echo "SCCACHE_GHA_RW_MODE=${cache_mode}"'
  ]
  required_sccache.each do |fragment|
    errors << "scripts/configure_ci_sccache.sh: missing authority/fallback contract #{fragment.inspect}" unless sccache.include?(fragment)
  end

  cargo_home = File.read(cargo_home_path)
  required_cargo_home = [
    '"${RUNNER_TEMP:-}"',
    'cargo_home="${RUNNER_TEMP}/finch-cargo-home"',
    'mkdir -p "${cargo_home}/bin"',
    'echo "CARGO_HOME=${cargo_home}"',
    'echo "${cargo_home}/bin"'
  ]
  required_cargo_home.each do |fragment|
    errors << "scripts/configure_ci_cargo_home.sh: missing isolated-home contract #{fragment.inspect}" unless cargo_home.include?(fragment)
  end
  if cargo_home.include?("rm -rf") || sccache.include?("rm -rf")
    errors << "CI cache helpers must not recursively delete paths derived from HOME or runner state"
  end
end

def validate(root)
  errors = []
  documents = {}
  KNOWN_CONSUMERS.each_key do |workflow|
    path = File.join(root, workflow)
    begin
      documents[workflow] = YAML.safe_load(File.read(path), aliases: true)
    rescue StandardError => error
      errors << "#{workflow}: cannot parse workflow YAML: #{error.message}"
    end
  end
  return errors unless errors.empty?

  documents.each do |workflow, document|
    jobs = document["jobs"] || {}
    jobs.each do |job_name, job|
      steps(job).each { |step| validate_cache_action(step, errors, workflow, job_name) }
      if !KNOWN_CONSUMERS.fetch(workflow).include?(job_name) && job_name != "cargo-download-cache" && first_compile_index(job)
        actionable(errors, workflow, job_name,
                   "new compiling Cargo job is outside the explicit cache-consumer contract")
      end
    end
    KNOWN_CONSUMERS.fetch(workflow).each do |job_name|
      job = jobs[job_name]
      if job.nil?
        actionable(errors, workflow, job_name, "required canonical expensive job is missing")
        next
      end
      validate_consumer(job, errors, workflow, job_name)
      if SCCACHE_CONSUMERS.fetch(workflow).include?(job_name)
        writer = %w[test build].include?(job_name) ? "${{ matrix.sccache_writer }}" : "false"
        validate_sccache(job, errors, workflow, job_name, writer)
      end
    end
  end

  ci_jobs = documents.dig(".github/workflows/ci.yml", "jobs") || {}
  validate_matrix(errors, ci_jobs)
  validate_release_matrix(errors, documents.dig(".github/workflows/release.yml", "jobs") || {})
  producer = ci_jobs["cargo-download-cache"]
  producer ? validate_download_producer(producer, errors) : actionable(errors, ".github/workflows/ci.yml", "cargo-download-cache", "trusted producer is missing")
  validate_audit(ci_jobs["security"] || {}, errors)
  validate_permissions(documents, errors)
  validate_helpers(root, errors)

  all_steps = documents.values.flat_map { |doc| (doc["jobs"] || {}).values.flat_map { |job| steps(job) } }
  saves = all_steps.select { |step| step["uses"] == CACHE_SAVE }
  errors << "canonical workflows: expected exactly two trusted cache save steps; found #{saves.length}" unless saves.length == 2
  audit_compiles = all_steps.count { |step| step["run"].to_s.include?("cargo install cargo-audit") }
  errors << "canonical workflows: cargo-audit must have exactly one cold compiler; found #{audit_compiles}" unless audit_compiles == 1
  forbidden_env = all_steps.flat_map { |step| (step["env"] || {}).keys }.grep(/\A(?:RUSTC_WRAPPER|SCCACHE_)/)
  errors << "canonical workflows: sccache authority must come only from configure_ci_sccache.sh; found #{forbidden_env.inspect}" unless forbidden_env.empty?

  errors
end

root = ARGV.fetch(0, ROOT)
errors = validate(root)
if errors.empty?
  puts "CI cache contract passed: exact downloads, four trusted sccache writer lanes, PR/release read-only"
  exit 0
end

warn "CI cache contract failed:"
errors.each { |error| warn "- #{error}" }
exit 1
