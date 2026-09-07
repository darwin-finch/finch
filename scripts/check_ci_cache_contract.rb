#!/usr/bin/env ruby
# frozen_string_literal: true

# Semantic production-boundary contract for Finch's canonical Cargo caches.

require "shellwords"
require "yaml"

ROOT = File.expand_path("..", __dir__)
CACHE_SHA = "0057852bfaa89a56745cba8c7296529d2fc39830"
CHECKOUT_SHA = "11bd71901bbe5b1630ceea73d27597364c9af683"
TOOLCHAIN_SHA = "62ae3a85dbdd2bedbb5819da8ce45635129289a1"
UPLOAD_SHA = "ea165f8d65b6e75b540449e92b4886f43607fa02"
DOWNLOAD_SHA = "d3f86a106a0bac45b974a628896c90dbdf5c8093"
CACHE_RESTORE = "actions/cache/restore@#{CACHE_SHA}"
CACHE_SAVE = "actions/cache/save@#{CACHE_SHA}"
CHECKOUT = "actions/checkout@#{CHECKOUT_SHA}"
APPROVED_ACTIONS = [
  CACHE_RESTORE,
  CACHE_SAVE,
  CHECKOUT,
  "dtolnay/rust-toolchain@#{TOOLCHAIN_SHA}",
  "actions/upload-artifact@#{UPLOAD_SHA}",
  "actions/download-artifact@#{DOWNLOAD_SHA}"
].freeze
TRUSTED_MAIN = "github.event_name == 'push' && github.ref == 'refs/heads/main'"
DOWNLOAD_PATHS = [
  "${{ runner.temp }}/finch-cargo-home/git/db",
  "${{ runner.temp }}/finch-cargo-home/registry/cache"
].freeze
OBJECT_PATH = "${{ runner.temp }}/finch-sccache-cache"
CONSUMERS = {
  ".github/workflows/ci.yml" => %w[test runtime-authority build security],
  ".github/workflows/release.yml" => %w[build-release]
}.freeze
COMPILE_SUBCOMMANDS = %w[audit bench build check clippy doc install metadata run rustc test tree].freeze

def steps(job)
  Array(job["steps"]).select { |step| step.is_a?(Hash) }
end

def disabled?(step)
  condition = step["if"]
  return true if condition == false

  %w[false ${{false}}].include?(condition.to_s.gsub(/\s+/, "").downcase)
end

def paths(step)
  step.dig("with", "path").to_s.lines.map(&:strip).reject(&:empty?).sort
end

def error(errors, workflow, job, message)
  errors << "#{workflow} job '#{job}': #{message}"
end

def split_shell(script)
  script.to_s.lines.flat_map { |line| line.split(/\s*(?:&&|\|\||;|\|)\s*/) }
end

def cargo_compile_script?(script)
  text = script.to_s
  text.scan(/\$\(([^()]*)\)/).any? { |match| cargo_compile_script?(match.first) } || begin
    variables = {}
    split_shell(text).any? do |statement|
      stripped = statement.strip
      next false if stripped.empty? || stripped.start_with?("#")
      tokens = Shellwords.shellsplit(stripped)
      while tokens.first&.match?(/\A[A-Za-z_][A-Za-z0-9_]*=.*/)
        name, value = tokens.shift.split("=", 2)
        variables[name] = value
      end
      next false if tokens.empty?
      executable = tokens.first
      variable = executable.match(/\A\$\{?([A-Za-z_][A-Za-z0-9_]*)\}?\z/)
      executable = variables[variable[1]] if variable
      shell = File.basename(executable.to_s)
      if %w[bash sh zsh].include?(shell) && tokens[1].to_s.match?(/\A-[a-z]*c[a-z]*\z/)
        next cargo_compile_script?(tokens[2].to_s)
      end
      next false if %w[echo printf].include?(executable)
      cargo_at = if executable == "cargo"
                   0
                 elsif %w[command env sudo timeout].include?(executable) || executable.to_s.end_with?("test_brains.sh")
                   tokens.index("cargo")
                 elsif executable.nil? || executable.to_s.start_with?("$")
                   0
                 end
      next false if cargo_at.nil?
      tokens[(cargo_at + 1)..].to_a.any? { |token| COMPILE_SUBCOMMANDS.include?(token) }
    rescue ArgumentError
      stripped.match?(/\bcargo\b.*\b(?:#{COMPILE_SUBCOMMANDS.join('|')})\b/)
    end
  end
end

def first_compile(job)
  steps(job).each_with_index do |step, index|
    next if disabled?(step)
    return index if cargo_compile_script?(step["run"])
  end
  nil
end

def selected(job, &block)
  steps(job).each_with_index.select { |step, _index| block.call(step) }
end

def exact_download_key?(key)
  value = key.to_s
  value.start_with?("cargo-downloads-v2-${{ runner.os }}-rust-1.98.0-config-") &&
    value.include?("hashFiles('rust-toolchain.toml', '.cargo/config', '.cargo/config.toml')") &&
    value.end_with?("-lock-${{ hashFiles('Cargo.lock') }}")
end

def validate_actions(documents, errors)
  documents.each do |workflow, document|
    (document["jobs"] || {}).each do |job_name, job|
      steps(job).each do |step|
        uses = step["uses"].to_s
        unless uses.empty? || APPROVED_ACTIONS.include?(uses)
          error(errors, workflow, job_name, "external action #{uses.inspect} is not an approved immutable full-SHA pin")
        end
        next unless uses.start_with?("actions/checkout@")
        error(errors, workflow, job_name, "checkout must use immutable #{CHECKOUT}") unless uses == CHECKOUT
        unless step.dig("with", "persist-credentials") == false
          error(errors, workflow, job_name, "checkout must set persist-credentials: false before build scripts")
        end
      end
    end
  end
end

def validate_permissions(documents, errors)
  ci = documents.fetch(".github/workflows/ci.yml")
  unless ci["permissions"] == { "contents" => "read" }
    errors << ".github/workflows/ci.yml: default permissions must be exactly contents: read"
  end
  (ci["jobs"] || {}).each do |name, job|
    error(errors, ".github/workflows/ci.yml", name, "must not elevate workflow permissions") if job.key?("permissions")
  end
end

def validate_matrices(documents, errors)
  ci = documents.fetch(".github/workflows/ci.yml").fetch("jobs")
  release = documents.fetch(".github/workflows/release.yml").fetch("jobs")
  producer_os = ci.dig("cargo-download-cache", "strategy", "matrix", "os")
  unless producer_os == %w[ubuntu-24.04 macos-14]
    error(errors, ".github/workflows/ci.yml", "cargo-download-cache",
          "download producer matrix must be exactly the two supported runner families; found #{producer_os.inspect}")
  end
  expected_tests = [
    { "os" => "ubuntu-24.04", "feature_name" => "default", "cargo_args" => "" },
    { "os" => "ubuntu-24.04", "feature_name" => "no-default-features", "cargo_args" => "--no-default-features" },
    { "os" => "macos-14", "feature_name" => "default", "cargo_args" => "" },
    { "os" => "macos-14", "feature_name" => "no-default-features", "cargo_args" => "--no-default-features" }
  ]
  actual_tests = ci.dig("test", "strategy", "matrix", "include")
  unless actual_tests == expected_tests
    error(errors, ".github/workflows/ci.yml", "test",
          "matrix must preserve two OSes and exactly one compiler-cache writer lane per OS; found #{actual_tests.inspect}")
  end
  expected_builds = [
    { "os" => "ubuntu-24.04", "target" => "x86_64-unknown-linux-gnu" },
    { "os" => "macos-14", "target" => "aarch64-apple-darwin" }
  ]
  actual_builds = ci.dig("build", "strategy", "matrix", "include")
  unless actual_builds == expected_builds
    error(errors, ".github/workflows/ci.yml", "build",
          "release writer matrix must be exactly the two supported target families; found #{actual_builds.inspect}")
  end
  expected_releases = [
    { "os" => "macos-14", "target" => "aarch64-apple-darwin", "asset_name" => "finch-macos-arm64" },
    { "os" => "ubuntu-24.04", "target" => "x86_64-unknown-linux-gnu", "asset_name" => "finch-linux-x86_64" }
  ]
  actual_releases = release.dig("build-release", "strategy", "matrix", "include")
  unless actual_releases == expected_releases
    error(errors, ".github/workflows/release.yml", "build-release",
          "release matrix must match the two trusted-main producer targets; found #{actual_releases.inspect}")
  end
end

def validate_download_restore(step, errors, workflow, job, producer: false)
  error(errors, workflow, job, "download restore must use #{CACHE_RESTORE}") unless step["uses"] == CACHE_RESTORE
  error(errors, workflow, job, "download restore must be nonfatal") unless step["continue-on-error"] == true
  error(errors, workflow, job,
        "download paths must be exactly #{DOWNLOAD_PATHS.inspect}; found #{paths(step).inspect}") unless paths(step) == DOWNLOAD_PATHS
  key = step.dig("with", "key")
  error(errors, workflow, job, "wrong download key #{key.inspect}") unless exact_download_key?(key)
  if producer
    error(errors, workflow, job, "producer restore must be lookup-only") unless step.dig("with", "lookup-only") == true
    error(errors, workflow, job, "producer must not restore a fallback union") if step.fetch("with", {}).key?("restore-keys")
  else
    expected = key.to_s.sub(/\$\{\{ hashFiles\('Cargo.lock'\) \}\}\z/, "")
    actual = step.dig("with", "restore-keys").to_s.strip
    error(errors, workflow, job,
          "consumer conservative restore prefix is wrong: #{actual.inspect}") unless actual == expected
    error(errors, workflow, job, "consumer download restore must be unconditional") unless step["if"].nil?
  end
end

def validate_consumers(documents, errors)
  CONSUMERS.each do |workflow, names|
    jobs = documents.fetch(workflow).fetch("jobs")
    names.each do |name|
      job = jobs[name]
      unless job
        error(errors, workflow, name, "required canonical expensive job is missing")
        next
      end
      compile_at = first_compile(job)
      unless compile_at
        error(errors, workflow, name, "has no active compiling Cargo command")
        next
      end
      homes = selected(job) { |step| step["run"].to_s.strip == "scripts/configure_ci_cargo_home.sh" }
      restores = selected(job) do |step|
        step.dig("with", "key").to_s.start_with?("cargo-downloads-")
      end
      error(errors, workflow, name, "needs exactly one isolated Cargo home setup; found #{homes.length}") unless homes.length == 1
      error(errors, workflow, name, "needs exactly one download restore; found #{restores.length}") unless restores.length == 1
      next unless homes.length == 1 && restores.length == 1
      validate_download_restore(restores.first.first, errors, workflow, name)
      unless homes.first.last < restores.first.last && restores.first.last < compile_at
        error(errors, workflow, name, "must configure Cargo home and restore downloads before compilation")
      end
      if steps(job).any? { |step| step["run"].to_s.include?("cargo generate-lockfile") }
        error(errors, workflow, name, "must use tracked Cargo.lock rather than online lock generation")
      end
    end

    jobs.each do |name, job|
      next if names.include?(name) || name == "cargo-download-cache"
      error(errors, workflow, name, "new compiling Cargo job is outside the cache contract") if first_compile(job)
    end
  end
end

def validate_download_producer(job, errors)
  workflow = ".github/workflows/ci.yml"
  name = "cargo-download-cache"
  error(errors, workflow, name, "must run only on trusted main") unless job["if"] == TRUSTED_MAIN
  restores = selected(job) { |step| step["uses"] == CACHE_RESTORE }
  fetches = selected(job) { |step| step["run"].to_s.strip == "cargo fetch --locked --verbose" }
  saves = selected(job) { |step| step["uses"] == CACHE_SAVE }
  unless [restores.length, fetches.length, saves.length] == [1, 1, 1]
    error(errors, workflow, name, "needs one lookup-only restore, complete fetch, and exact save")
    return
  end
  restore, restore_at = restores.first
  fetch, fetch_at = fetches.first
  save, save_at = saves.first
  validate_download_restore(restore, errors, workflow, name, producer: true)
  error(errors, workflow, name, "fetch must run exactly on lookup miss") unless fetch["if"] == "steps.cargo-downloads.outputs.cache-hit != 'true'"
  expected_save = "steps.cargo-downloads.outputs.cache-hit != 'true' && #{TRUSTED_MAIN}"
  error(errors, workflow, name, "save trust/miss condition is wrong: #{save['if'].inspect}") unless save["if"] == expected_save
  error(errors, workflow, name, "save must be nonfatal") unless save["continue-on-error"] == true
  error(errors, workflow, name, "save paths are wrong: #{paths(save).inspect}") unless paths(save) == DOWNLOAD_PATHS
  unless save.dig("with", "key") == "${{ steps.cargo-downloads.outputs.cache-primary-key }}"
    error(errors, workflow, name, "save must reuse lookup primary key")
  end
  error(errors, workflow, name, "must lookup before fetching and save only afterward") unless restore_at < fetch_at && fetch_at < save_at
end

def object_condition(name)
  return TRUSTED_MAIN unless name == "test-save"
  "success() && #{TRUSTED_MAIN} && matrix.feature_name == 'default' && steps.sccache-local.outputs.cache-hit != 'true' && steps.sccache-stop.outputs.save-ready == 'true'"
end

def validate_object_job(job, errors, name, profile:, target: false, save: false)
  workflow = ".github/workflows/ci.yml"
  native = selected(job) { |step| step["run"].to_s.strip == "python3 scripts/ci_native_cache_key.py" }
  restore = selected(job) { |step| paths(step) == [OBJECT_PATH] && step["uses"] == CACHE_RESTORE }
  configure = selected(job) { |step| step["run"].to_s.strip == "scripts/configure_ci_sccache.sh" }
  stop = selected(job) { |step| step["run"].to_s.strip == "scripts/stop_ci_sccache.sh" }
  unless [native.length, restore.length, configure.length, stop.length] == [1, 1, 1, 1]
    error(errors, workflow, name, "needs one native identity, local object restore, fixed-digest setup, and stop/measure")
    return
  end
  [native.first.first, restore.first.first, configure.first.first, stop.first.first].each do |step|
    error(errors, workflow, name, "compiler-object step #{step['name'].inspect} must be trusted-main-only") unless step["if"] == TRUSTED_MAIN
  end
  cache = restore.first.first
  error(errors, workflow, name, "object restore must be nonfatal") unless cache["continue-on-error"] == true
  key = cache.dig("with", "key").to_s
  required = ["sccache-local-v3-${{ runner.os }}", "profile-#{profile}", "cap-256m", "sccache-0.17.0", "rust-1.98.0", "native-${{ steps.native-cache.outputs.digest }}", "hashFiles('Cargo.lock')", ".cargo/config", ".cargo/config.toml"]
  required << "target-${{ matrix.target }}" if target
  missing = required.reject { |part| key.include?(part) }
  error(errors, workflow, name, "object key #{key.inspect} misses #{missing.inspect}") unless missing.empty?
  prefix = cache.dig("with", "restore-keys").to_s.strip
  error(errors, workflow, name, "object fallback must stay inside the exact compatible family") unless prefix == key.sub(/\$\{\{ hashFiles\('Cargo.lock'\) \}\}\z/, "")
  compile_at = first_compile(job)
  unless native.first.last < restore.first.last && restore.first.last < configure.first.last && configure.first.last < compile_at && compile_at < stop.first.last
    error(errors, workflow, name, "compiler cache setup/compile/stop ordering is wrong")
  end

  saves = selected(job) { |step| paths(step) == [OBJECT_PATH] && step["uses"] == CACHE_SAVE }
  expected_count = save ? 1 : 0
  error(errors, workflow, name, "expected #{expected_count} compiler-object save; found #{saves.length}") unless saves.length == expected_count
  return unless save && saves.length == 1
  save_step, save_at = saves.first
  expected = name == "test" ? object_condition("test-save") : "success() && #{TRUSTED_MAIN} && steps.sccache-local.outputs.cache-hit != 'true' && steps.sccache-stop.outputs.save-ready == 'true'"
  error(errors, workflow, name, "object save trust/order predicate is wrong: #{save_step['if'].inspect}") unless save_step["if"] == expected
  error(errors, workflow, name, "object save must be nonfatal") unless save_step["continue-on-error"] == true
  error(errors, workflow, name, "object save must reuse restore primary key") unless save_step.dig("with", "key") == "${{ steps.sccache-local.outputs.cache-primary-key }}"
  error(errors, workflow, name, "object save must follow successful stop/measure") unless stop.first.last < save_at
end

def validate_objects(documents, errors)
  ci = documents.dig(".github/workflows/ci.yml", "jobs")
  validate_object_job(ci.fetch("test"), errors, "test", profile: "debug", save: true)
  validate_object_job(ci.fetch("runtime-authority"), errors, "runtime-authority", profile: "debug")
  validate_object_job(ci.fetch("build"), errors, "build", profile: "release-lto-false-codegen-16", target: true, save: true)
  release_steps = documents.dig(".github/workflows/release.yml", "jobs", "build-release").then { |job| steps(job) }
  if release_steps.any? { |step| paths(step).include?(OBJECT_PATH) || step["run"].to_s.include?("sccache") }
    error(errors, ".github/workflows/release.yml", "build-release", "tag builds must not restore or execute compiler caches")
  end
  all_steps = documents.values.flat_map { |doc| (doc["jobs"] || {}).values.flat_map { |job| steps(job) } }
  if all_steps.any? { |step| step["uses"].to_s.include?("sccache-action") || step["run"].to_s.include?("SCCACHE_GHA") || (step["env"] || {}).keys.any? { |key| key.start_with?("SCCACHE_GHA") } }
    errors << "canonical workflows must not expose GitHub cache credentials to sccache"
  end
  injections = all_steps.select do |step|
    run = step["run"].to_s
    run.include?("GITHUB_ENV") || run.match?(/(?:RUSTC_WRAPPER|SCCACHE_(?:DIR|CACHE|LOCAL|GHA))/)
  end
  errors << "canonical workflows must configure compiler authority only inside reviewed helpers" unless injections.empty?
  object_saves = all_steps.count { |step| paths(step) == [OBJECT_PATH] && step["uses"] == CACHE_SAVE }
  errors << "canonical workflows must have exactly two compiler save definitions; found #{object_saves}" unless object_saves == 2
end

def validate_audit(job, errors)
  workflow = ".github/workflows/ci.yml"
  name = "security"
  install = selected(job) { |step| step["run"].to_s.strip == "scripts/configure_ci_cargo_audit.sh" }
  verify = selected(job) { |step| step["run"].to_s.include?("actual=$(cargo-audit --version)") }
  audit = selected(job) { |step| step["run"].to_s.strip == "cargo audit" }
  forbidden = steps(job).any? do |step|
    step["run"].to_s.include?("cargo install cargo-audit") ||
      step.dig("with", "path").to_s.include?("cargo-audit") ||
      step.dig("with", "key").to_s.include?("cargo-audit")
  end
  error(errors, workflow, name, "must not compile or cache the cargo-audit executable") if forbidden
  unless [install.length, verify.length, audit.length] == [1, 1, 1]
    error(errors, workflow, name, "needs one active fixed-digest audit install/verify/run sequence")
    return
  end
  install_step, install_at = install.first
  verify_step, verify_at = verify.first
  _audit_step, audit_at = audit.first
  error(errors, workflow, name, "fixed-digest audit install must be unconditional") unless install_step["if"].nil?
  error(errors, workflow, name, "audit verification must be unconditional") unless verify_step["if"].nil?
  unless verify_step["run"].to_s.include?('[[ "$actual" != "cargo-audit 0.22.2" ]]')
    error(errors, workflow, name, "audit verification must require exact version 0.22.2")
  end
  error(errors, workflow, name, "audit install/verify/run ordering is wrong") unless install_at < verify_at && verify_at < audit_at
end

def validate_release(documents, errors)
  release = documents.fetch(".github/workflows/release.yml")
  error(errors, ".github/workflows/release.yml", "workflow", "default permissions must be contents: read") unless release["permissions"] == { "contents" => "read" }
  build = release.dig("jobs", "build-release")
  unless build["env"] == nil && release["env"]["CARGO_PROFILE_RELEASE_LTO"] == "false" && release["env"]["CARGO_PROFILE_RELEASE_CODEGEN_UNITS"] == "16"
    error(errors, ".github/workflows/release.yml", "build-release", "release profile must be lto=false/codegen-units=16")
  end
  ci_build = documents.dig(".github/workflows/ci.yml", "jobs", "build", "env")
  expected_profile = { "CARGO_PROFILE_RELEASE_LTO" => "false", "CARGO_PROFILE_RELEASE_CODEGEN_UNITS" => "16" }
  error(errors, ".github/workflows/ci.yml", "build", "trusted release writer profile does not match tag build: #{ci_build.inspect}") unless ci_build == expected_profile
  create = release.dig("jobs", "create-release")
  error(errors, ".github/workflows/release.yml", "create-release", "must alone hold contents: write") unless create["permissions"] == { "contents" => "write" }
  release_step = steps(create).find { |step| step["run"].to_s.include?("gh release create") }
  unless release_step && release_step.dig("env", "GH_REPO") == "${{ github.repository }}"
    error(errors, ".github/workflows/release.yml", "create-release", "no-checkout release creation must set GH_REPO to github.repository")
  end
end

def validate_helpers(root, errors)
  required = %w[configure_ci_cargo_home.sh configure_ci_sccache.sh stop_ci_sccache.sh ci_native_cache_key.py configure_ci_cargo_audit.sh ci_rustc_cache_wrapper.sh]
  required.each do |name|
    path = File.join(root, "scripts", name)
    errors << "scripts/#{name}: helper must exist and be executable" unless File.executable?(path)
  end
  return unless required.all? { |name| File.file?(File.join(root, "scripts", name)) }
  setup = File.read(File.join(root, "scripts/configure_ci_sccache.sh"))
  stop = File.read(File.join(root, "scripts/stop_ci_sccache.sh"))
  native = File.read(File.join(root, "scripts/ci_native_cache_key.py"))
  audit = File.read(File.join(root, "scripts/configure_ci_cargo_audit.sh"))
  wrapper = File.read(File.join(root, "scripts/ci_rustc_cache_wrapper.sh"))
  required_setup = [
    "67c4a96dd237c1f518f6b36083f270f9976d516f1e57fce891755ea782e50006",
    "0c560bfba31aef5bdfb4fb3d2677f6e61d71c5c00952f2a83344f47aa31f00f1",
    "SCCACHE_CACHE_SIZE=256M", "SCCACHE_LOCAL_RW_MODE=READ_WRITE",
    '"${GITHUB_EVENT_NAME:-}" != "push"', '"${GITHUB_REF:-}" != "refs/heads/main"',
    "archive has unexpected members", "contains a link or special file", "--max-filesize 12582912",
    "reviewed Linux v0.17.0 archive is 9,561,816 bytes",
    "scripts/ci_rustc_cache_wrapper.sh"
  ]
  required_setup.each { |part| errors << "scripts/configure_ci_sccache.sh: missing #{part.inspect}" unless setup.include?(part) }
  errors << "scripts/configure_ci_sccache.sh: remote GHA backend is forbidden" if setup.include?("SCCACHE_GHA")
  %w[--show-stats --stop-server 262144 save-ready=true].each do |part|
    errors << "scripts/stop_ci_sccache.sh: missing stop/cap proof #{part.inspect}" unless stop.include?(part)
  end
  %w[RUNNER_OS RUNNER_ARCH GITHUB_OUTPUT capnp clang xcrun cc ld sha256].each do |part|
    errors << "scripts/ci_native_cache_key.py: missing native identity input #{part.inspect}" unless native.include?(part)
  end
  required_audit = [
    "ab28a1bdb54db4d5d8ad5981cf1f959410370b3d28250dbd35f6a44248620e39",
    "cargo-audit-x86_64-unknown-linux-gnu-v0.22.2.tgz", "shasum -a 256 --check --status",
    "archive has unexpected members", "contains a link or special file", "--max-filesize 8388608"
  ]
  required_audit.each { |part| errors << "scripts/configure_ci_cargo_audit.sh: missing #{part.inspect}" unless audit.include?(part) }
  %w[--test|build_script_build|build-script-build --crate-type=*bin* SCCACHE_PATH].each do |part|
    errors << "scripts/ci_rustc_cache_wrapper.sh: missing executable exclusion #{part.inspect}" unless wrapper.include?(part)
  end
end

def validate_tracked_lock_dependents(root, errors)
  issue_185 = File.read(File.join(root, ".github/workflows/issue-185-spreadsheet-advisories.yml"))
  if issue_185.include?("Cargo.lock is gitignored") || !issue_185.include?("cargo tree --locked")
    errors << ".github/workflows/issue-185-spreadsheet-advisories.yml: must consume the tracked lock with cargo tree --locked"
  end

  issue_186 = File.read(File.join(root, ".github/workflows/issue-186-ssh-removal.yml"))
  required = ["cargo metadata --locked", "cargo tree --locked", "git diff --exit-code -- Cargo.lock"]
  missing = required.reject { |part| issue_186.include?(part) }
  unless missing.empty?
    errors << ".github/workflows/issue-186-ssh-removal.yml: tracked-lock validation misses #{missing.inspect}"
  end
  if issue_186.include?("cargo generate-lockfile")
    errors << ".github/workflows/issue-186-ssh-removal.yml: must validate rather than rewrite the tracked lock"
  end
  unlocked = issue_186.scan(/cargo (?:metadata|tree|test|build)\b[^\n]*/).reject { |command| command.include?("--locked") }
  unless unlocked.empty?
    errors << ".github/workflows/issue-186-ssh-removal.yml: Cargo graph/build commands must use --locked; found #{unlocked.inspect}"
  end

  Dir.glob(File.join(root, ".github/workflows/*.{yml,yaml}")).sort.each do |path|
    next unless File.read(path).include?("cargo install cargo-audit")

    errors << "#{path.delete_prefix("#{root}/")}: must use the fixed-digest cargo-audit helper rather than compile cargo-audit"
  end
end

def validate(root)
  errors = []
  documents = {}
  CONSUMERS.each_key do |workflow|
    begin
      documents[workflow] = YAML.safe_load(File.read(File.join(root, workflow)), aliases: true)
    rescue StandardError => exception
      errors << "#{workflow}: cannot parse YAML: #{exception.message}"
    end
  end
  return errors unless errors.empty?
  unless File.file?(File.join(root, "Cargo.lock"))
    errors << "Cargo.lock must be tracked for restore-before-resolution correctness"
  end
  ignore_lines = File.readlines(File.join(root, ".gitignore"), chomp: true).map(&:strip).reject { |line| line.empty? || line.start_with?("#") }
  if ignore_lines.any? { |line| %w[Cargo.lock /Cargo.lock **/Cargo.lock].include?(line) }
    errors << "Cargo.lock must not be ignored"
  end

  validate_actions(documents, errors)
  validate_permissions(documents, errors)
  validate_matrices(documents, errors)
  validate_consumers(documents, errors)
  ci_jobs = documents.dig(".github/workflows/ci.yml", "jobs") || {}
  producer = ci_jobs["cargo-download-cache"]
  producer ? validate_download_producer(producer, errors) : error(errors, ".github/workflows/ci.yml", "cargo-download-cache", "producer is missing")
  validate_objects(documents, errors)
  validate_audit(ci_jobs.fetch("security", {}), errors)
  validate_release(documents, errors)
  validate_helpers(root, errors)
  validate_tracked_lock_dependents(root, errors)
  errors
end

root = ARGV.fetch(0, ROOT)
errors = validate(root)
if errors.empty?
  puts "CI cache contract passed: tracked lock, authenticated PR downloads, trusted-main-only 4x256MiB compiler families"
  exit 0
end
warn "CI cache contract failed:"
errors.each { |entry| warn "- #{entry}" }
exit 1
