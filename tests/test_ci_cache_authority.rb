#!/usr/bin/env ruby
# frozen_string_literal: true

require "minitest/autorun"
require "open3"
require "psych"
require "tempfile"

ROOT = File.expand_path("..", __dir__)
WORKFLOW = File.join(ROOT, ".github/workflows/ci.yml")
CHECKER = File.join(ROOT, "scripts/check_ci_cache_authority.rb")
CACHE_SHA = "0057852bfaa89a56745cba8c7296529d2fc39830"
RESTORE_ACTION = "actions/cache/restore@#{CACHE_SHA}"
SAVE_ACTION = "actions/cache/save@#{CACHE_SHA}"

class CiCacheAuthorityTest < Minitest::Test
  def setup
    @document = Psych.safe_load(File.read(WORKFLOW), aliases: false)
  end

  def test_checked_in_workflow_passes
    stdout, stderr, status = Open3.capture3("ruby", CHECKER, chdir: ROOT)
    assert status.success?, "checked-in cache contract must pass\nstdout:\n#{stdout}\nstderr:\n#{stderr}"
  end

  def test_toolchain_contract_wires_checker_and_mutations_exactly_once
    contract = File.readlines(File.join(ROOT, "tests/toolchain_contract.sh"), chomp: true)
    [
      "ruby tests/test_ci_cache_authority.rb",
      "ruby scripts/check_ci_cache_authority.rb"
    ].each do |invocation|
      assert_equal 1, contract.count(invocation),
                   "toolchain contract must contain exactly one executable #{invocation.inspect}; lines=#{contract.grep(/ci_cache_authority/).inspect}"
    end
  end

  def test_mutations_are_rejected
    mutations.each do |name, (expected_diagnostic, mutate)|
      document = Marshal.load(Marshal.dump(@document))
      mutate.call(document)
      stdout, stderr, status = run_checker(document)
      refute status.success?, "mutation #{name.inspect} escaped the cache contract\nstdout:\n#{stdout}\nstderr:\n#{stderr}\njob:\n#{Psych.dump(test_job(document))}"
      assert_includes stderr, expected_diagnostic,
                      "mutation #{name.inspect} failed without its actionable diagnostic\nstderr:\n#{stderr}"
      assert_includes stderr, "parsed jobs.test state:",
                      "mutation #{name.inspect} must dump parsed job state\nstderr:\n#{stderr}"
    end
  end

  private

  def test_job(document)
    document.fetch("jobs").fetch("test")
  end

  def steps(document)
    test_job(document).fetch("steps")
  end

  def restore(document)
    steps(document).find { |step| step["uses"] == RESTORE_ACTION }
  end

  def save(document)
    steps(document).find { |step| step["uses"] == SAVE_ACTION }
  end

  def named(document, name)
    steps(document).find { |step| step["name"] == name }
  end

  def run_checker(document)
    Tempfile.create(["ci-cache-authority", ".yml"]) do |file|
      file.write(Psych.dump(document))
      file.flush
      return Open3.capture3("ruby", CHECKER, file.path, chdir: ROOT)
    end
  end

  def mutations
    {
      "missing restore" => ["action identities/order", ->(doc) { steps(doc).delete(restore(doc)) }],
      "duplicate restore" => ["action identities/order", ->(doc) { steps(doc).insert(5, Marshal.load(Marshal.dump(restore(doc)))) }],
      "missing save" => ["action identities/order", ->(doc) { steps(doc).delete(save(doc)) }],
      "duplicate save" => ["action identities/order", ->(doc) { steps(doc) << Marshal.load(Marshal.dump(save(doc))) }],
      "pull request capable save" => ["trusted successful-main miss-only condition", ->(doc) { save(doc)["if"] = "${{ success() && github.event_name == 'pull_request' }}" }],
      "tag capable save" => ["trusted successful-main miss-only condition", ->(doc) { save(doc)["if"] = "${{ success() && github.event_name == 'push' && startsWith(github.ref, 'refs/tags/') }}" }],
      "missing success gate" => ["trusted successful-main miss-only condition", ->(doc) { save(doc)["if"] = save(doc)["if"].sub("success() && ", "") }],
      "missing main gate" => ["trusted successful-main miss-only condition", ->(doc) { save(doc)["if"] = save(doc)["if"].sub("github.ref == 'refs/heads/main' && ", "") }],
      "missing exact-hit gate" => ["trusted successful-main miss-only condition", ->(doc) { save(doc)["if"] = save(doc)["if"].sub(" && steps.cargo-cache-restore.outputs.cache-hit != 'true'", "") }],
      "condition or bypass" => ["trusted successful-main miss-only condition", ->(doc) { save(doc)["if"] = save(doc)["if"].sub(" }}", " || github.event_name == 'pull_request' }}") }],
      "disabled restore" => ["cache restore must be unconditional", ->(doc) { restore(doc)["if"] = false }],
      "disabled save" => ["trusted successful-main miss-only condition", ->(doc) { save(doc)["if"] = "${{ false }}" }],
      "restore failure becomes fatal" => ["cache restore continue-on-error", ->(doc) { restore(doc)["continue-on-error"] = false }],
      "save failure becomes fatal" => ["cache save continue-on-error", ->(doc) { save(doc)["continue-on-error"] = false }],
      "mutable restore action" => ["action identities/order", ->(doc) { restore(doc)["uses"] = "actions/cache/restore@v4" }],
      "mutable save action" => ["action identities/order", ->(doc) { save(doc)["uses"] = "actions/cache/save@v4" }],
      "replacement cache action" => ["action identities/order", ->(doc) { restore(doc)["uses"] = "Swatinem/rust-cache@v2" }],
      "additional cache action" => ["action identities/order", ->(doc) { steps(doc).insert(5, { "name" => "Alternate cache", "uses" => "actions/cache@v4" }) }],
      "save before final workload" => ["step names/order", lambda { |doc|
        cache_save = steps(doc).pop
        steps(doc).insert(8, cache_save)
      }],
      "step after save" => ["step names/order", ->(doc) { steps(doc) << { "name" => "Late workload", "run" => "cargo check" } }],
      "recomputed save key" => ["primary-key handoff", ->(doc) { save(doc).fetch("with")["key"] = restore(doc).fetch("with").fetch("key") }],
      "restore path drift" => ["cache restore paths", ->(doc) { restore(doc).fetch("with")["path"] = "~/.cargo/registry\ntarget\n" }],
      "save path drift" => ["cache save paths", ->(doc) { save(doc).fetch("with")["path"] = "~/.cargo/registry\n~/.cargo/git\n" }],
      "primary key drift" => ["cache restore primary key", ->(doc) { restore(doc).fetch("with")["key"] = "drifted" }],
      "restore prefix drift" => ["cache restore prefix", ->(doc) { restore(doc).fetch("with")["restore-keys"] = "broad-prefix-\n" }],
      "matrix row removed" => ["exact four OS/feature rows", ->(doc) { test_job(doc).dig("strategy", "matrix", "include").pop }],
      "matrix dimension added" => ["exact four OS/feature rows", ->(doc) { test_job(doc).dig("strategy", "matrix", "include").first["target"] = "host" }],
      "matrix cargo args drift" => ["exact four OS/feature rows", ->(doc) { test_job(doc).dig("strategy", "matrix", "include").last["cargo_args"] = "--all-features" }],
      "missing workload" => ["step names/order", ->(doc) { steps(doc).delete(named(doc, "Compile all targets")) }],
      "renamed workload" => ["step names/order", ->(doc) { named(doc, "Test all targets")["name"] = "Tests" }],
      "workload shell trick" => ["executable fields", ->(doc) { named(doc, "Build binary")["shell"] = "bash" }],
      "workload command substitution" => ["executable fields", ->(doc) { named(doc, "Build binary")["run"] = "echo \"$(cargo build --bin finch)\"" }],
      "job defaults trick" => ["must not define defaults", ->(doc) { test_job(doc)["defaults"] = { "run" => { "shell" => "bash" } } }],
      "job disabled" => ["job-level if", ->(doc) { test_job(doc)["if"] = false }],
      "restore lookup only" => ["cache restore inputs", ->(doc) { restore(doc).fetch("with")["lookup-only"] = true }],
      "restore id renamed" => ["stable id", ->(doc) { restore(doc)["id"] = "cache" }]
    }
  end
end
