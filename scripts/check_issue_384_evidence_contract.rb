#!/usr/bin/env ruby
# frozen_string_literal: true

# TEMPORARY_FINCH_ISSUE_384_CACHE_EVIDENCE_CONTRACT_BEGIN
# Delete this checker with the temporary issue 384 evidence workflow.

require "yaml"

path = ARGV.fetch(0, File.expand_path("../.github/workflows/temporary-issue-384-cache-evidence.yml", __dir__))
helper_path = ARGV.fetch(1, File.expand_path("configure_temporary_issue_384_sccache.sh", __dir__))
source = File.read(path)
helper = File.read(helper_path)
document = YAML.safe_load(source, aliases: true)
errors = []
expected_condition = "github.repository == 'darwin-finch/finch' && github.event_name == 'pull_request' && github.event.pull_request.base.ref == 'main' && github.event.pull_request.head.repo.full_name == github.repository && github.event.pull_request.head.ref == 'codex/issue-384-ci-cache-v3' && github.actor == 'schancel' && github.triggering_actor == 'schancel'"
claim_prefix = "finch-evidence-only-issue384-claim-5f155aeb-2a15-4837-8590-833b4a4daa06-run-${{ github.run_id }}-"
cache_sha = "0057852bfaa89a56745cba8c7296529d2fc39830"
approved_actions = [
  "actions/checkout@11bd71901bbe5b1630ceea73d27597364c9af683",
  "dtolnay/rust-toolchain@62ae3a85dbdd2bedbb5819da8ce45635129289a1",
  "actions/cache/restore@#{cache_sha}",
  "actions/cache/save@#{cache_sha}"
]

unless source.start_with?("# TEMPORARY_FINCH_ISSUE_384_CACHE_EVIDENCE_BEGIN\n") &&
       source.end_with?("# TEMPORARY_FINCH_ISSUE_384_CACHE_EVIDENCE_END\n")
  errors << "temporary workflow must retain mechanically verifiable boundary markers"
end
unless document[true] == { "pull_request" => { "branches" => ["main"] } }
  errors << "temporary workflow trigger must be only pull_request to main"
end
errors << "temporary workflow permissions must be exactly contents: read" unless document["permissions"] == { "contents" => "read" }
jobs = document["jobs"] || {}
errors << "temporary workflow must contain exactly one evidence job" unless jobs.keys == ["controlled-cache-evidence"]
job = jobs["controlled-cache-evidence"] || {}
errors << "evidence authority predicate changed: #{job['if'].inspect}" unless job["if"] == expected_condition
errors << "evidence runner must stay ubuntu-24.04" unless job["runs-on"] == "ubuntu-24.04"
steps = Array(job["steps"])

steps.each do |step|
  uses = step["uses"].to_s
  next if uses.empty? || approved_actions.include?(uses)

  errors << "temporary workflow action is not an approved immutable pin: #{uses.inspect}"
end
checkout = steps.find { |step| step["uses"].to_s.start_with?("actions/checkout@") }
unless checkout&.dig("with", "ref") == "${{ github.event.pull_request.head.sha }}" &&
       checkout&.dig("with", "persist-credentials") == false
  errors << "checkout must select the exact PR head SHA without persisted credentials"
end

restores = steps.select { |step| step["uses"] == "actions/cache/restore@#{cache_sha}" }
saves = steps.select { |step| step["uses"] == "actions/cache/save@#{cache_sha}" }
errors << "evidence workflow needs exactly two restore and two save actions" unless restores.length == 2 && saves.length == 2
restores.each do |step|
  key = step.dig("with", "key").to_s
  errors << "evidence cache key escaped claim/run namespace: #{key.inspect}" unless key.start_with?(claim_prefix)
  errors << "evidence restores must be exact-only" if step.fetch("with", {}).key?("restore-keys")
  errors << "evidence restore failures must preserve ordinary-build fallback" unless step["continue-on-error"] == true
end
if source.include?("cargo-downloads-v2-") || source.include?("sccache-local-v3-")
  errors << "temporary evidence must never address a production cache namespace"
end

cargo_restore = restores.find { |step| step["id"] == "evidence-cargo" }
compiler_restore = restores.find { |step| step["id"] == "evidence-sccache" }
cargo_save = saves.find { |step| step.dig("with", "key") == "${{ steps.evidence-cargo.outputs.cache-primary-key }}" }
compiler_save = saves.find { |step| step.dig("with", "key") == "${{ steps.evidence-sccache.outputs.cache-primary-key }}" }
errors << "Cargo evidence restore/save identity is incomplete" unless cargo_restore && cargo_save
errors << "compiler evidence restore/save identity is incomplete" unless compiler_restore && compiler_save
unless cargo_restore&.dig("with", "key").to_s.include?("-cargo-${{ runner.os }}-") &&
       cargo_restore&.dig("with", "key").to_s.include?("hashFiles('Cargo.lock')")
  errors << "Cargo evidence key must bind OS and tracked lock"
end
unless compiler_restore&.dig("with", "key").to_s.include?("-sccache-${{ runner.os }}-profile-debug-cap-256m-") &&
       compiler_restore&.dig("with", "key").to_s.include?("native-${{ steps.native-cache.outputs.digest }}")
  errors << "compiler evidence key must bind OS, profile, cap, and native identity"
end
errors << "Cargo evidence save must run only on exact miss" unless cargo_save&.dig("if") == "steps.evidence-cargo.outputs.cache-hit != 'true'"
expected_compiler_save = "success() && steps.evidence-sccache.outputs.cache-hit != 'true' && steps.sccache-config.outputs.enabled == 'true' && steps.sccache-stop.outputs.save-ready == 'true'"
errors << "compiler evidence save must require miss, setup, success, and measured cap" unless compiler_save&.dig("if") == expected_compiler_save
saves.each do |step|
  errors << "evidence save failures must preserve the measured build result" unless step["continue-on-error"] == true
end

fetch = steps.find { |step| step["run"].to_s.strip == "cargo fetch --locked --verbose" }
errors << "cold Cargo evidence must fetch the tracked graph only on miss" unless fetch&.dig("if") == "steps.evidence-cargo.outputs.cache-hit != 'true'"
configure = steps.find { |step| step["run"].to_s.strip == "scripts/configure_temporary_issue_384_sccache.sh" }
unless configure&.dig("env") == { "FINCH_ISSUE_384_EVIDENCE" => "5f155aeb-2a15-4837-8590-833b4a4daa06" }
  errors << "temporary sccache helper override changed"
end
compiles = steps.select { |step| step["run"].to_s.include?("cargo check --locked --lib") }
targets = compiles.map { |step| step.dig("env", "CARGO_TARGET_DIR") }
unless compiles.length == 2 && targets.uniq.length == 2 && targets.all? { |target| target.to_s.include?("github.run_attempt") }
  errors << "evidence must compile twice into distinct run-attempt-scoped target directories"
end
stop = steps.find { |step| step["run"].to_s.strip == "scripts/stop_ci_sccache.sh" }
if cargo_restore && fetch && cargo_save && compiler_restore && configure && compiles.length == 2 && stop && compiler_save
  order = [cargo_restore, fetch, cargo_save, compiler_restore, configure, compiles[0], compiles[1], stop, compiler_save].map { |step| steps.index(step) }
  errors << "evidence restore/fetch/save/compile/measure ordering changed: #{order.inspect}" unless order == order.sort && order.uniq.length == order.length
else
  errors << "evidence ordering cannot be proven because a required step is missing"
end
%w[job-start cargo-restore-finished compiler-restore-finished first-compile-start first-compile-finish second-compile-start second-compile-finish job-finish].each do |timestamp|
  errors << "evidence log misses timestamp #{timestamp}" unless source.include?(timestamp)
end
%w[cargo-cache-hit compiler-cache-hit compiler-cache-size-kib --show-stats].each do |diagnostic|
  errors << "evidence log misses diagnostic #{diagnostic}" unless source.include?(diagnostic)
end
unless helper.start_with?("#!/usr/bin/env bash\n# TEMPORARY_FINCH_ISSUE_384_SCCACHE_AUTHORITY_BEGIN\n") &&
       helper.end_with?("# TEMPORARY_FINCH_ISSUE_384_SCCACHE_AUTHORITY_END\n")
  errors << "temporary sccache authority helper must retain deletion markers"
end
[
  '"${FINCH_ISSUE_384_EVIDENCE:-}" != "${expected_claim}"',
  '"${GITHUB_REPOSITORY:-}" != "darwin-finch/finch"',
  '"${GITHUB_EVENT_NAME:-}" != "pull_request"',
  '"${GITHUB_BASE_REF:-}" != "main"',
  '"${GITHUB_HEAD_REF:-}" != "codex/issue-384-ci-cache-v3"',
  '"${GITHUB_ACTOR:-}" != "schancel"',
  '"${GITHUB_TRIGGERING_ACTOR:-}" != "schancel"',
  "GITHUB_EVENT_NAME=push GITHUB_REF=refs/heads/main"
].each do |guard|
  errors << "temporary sccache authority helper misses #{guard.inspect}" unless helper.include?(guard)
end

if errors.empty?
  puts "Temporary issue 384 evidence contract passed: same-repo schancel PR, disjoint claim/run caches, bounded compiler save"
  exit 0
end
warn "Temporary issue 384 evidence contract failed:"
errors.each { |error| warn "- #{error}" }
exit 1

# TEMPORARY_FINCH_ISSUE_384_CACHE_EVIDENCE_CONTRACT_END
