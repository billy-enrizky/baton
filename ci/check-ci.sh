#!/usr/bin/env bash
set -euo pipefail

workflow=${1:?usage: ci/check-ci.sh <workflow>}
ruby -ryaml -e '
def fail!(message)
  warn "ERROR: #{message}"
  exit 1
end

path = ARGV.fetch(0)
document = YAML.load_file(path)
jobs = document["jobs"]
fail!("workflow has no jobs") unless jobs.is_a?(Hash)

on = document["on"] || document[true]
events = on.is_a?(Hash) ? on.keys.map(&:to_s) : Array(on).map(&:to_s)
%w[pull_request push merge_group workflow_dispatch].each do |event|
  fail!("missing trigger #{event}") unless events.include?(event)
end
push = on.is_a?(Hash) ? on["push"] : nil
branches = push.is_a?(Hash) ? Array(push["branches"]).map(&:to_s) : []
fail!("push trigger must target main") unless branches.include?("main")

fail!("workflow permissions must be contents: read") unless document.dig("permissions", "contents") == "read"
fail!("pull_request_target is not allowed") if events.include?("pull_request_target")

group = document.dig("concurrency", "group").to_s
cancel = document.dig("concurrency", "cancel-in-progress").to_s
fail!("concurrency must key by PR number or commit") unless group.include?("pull_request.number") && group.include?("github.sha")
fail!("pull requests must cancel superseded runs") unless cancel.include?("pull_request")

jobs.each do |name, job|
  fail!("job #{name} has no timeout-minutes") unless job.key?("timeout-minutes")
  timeout = job["timeout-minutes"]
  fail!("job #{name} timeout must be numeric and below 360") unless timeout.is_a?(Numeric) && timeout.positive? && timeout < 360

  steps = Array(job["steps"])
  steps.each do |step|
    uses = step["uses"].to_s
    fail!("job #{name} uses a retry action") if uses.match?(/retry/i)
    fail!("job #{name} uses continue-on-error") if step["continue-on-error"] == true
    run = step["run"].to_s
    fail!("job #{name} masks a shell failure with || true") if run.include?("|| true")
    fail!("job #{name} masks a shell failure with set +e") if run.include?("set +e") || run.include?("set +o errexit")
  end
end

verify = jobs["verify"]
fail!("missing verify job") unless verify.is_a?(Hash)
fail!("verify must use if: always()") unless verify["if"].to_s.include?("always()")
needs = Array(verify["needs"]).map(&:to_s)
expected = jobs.keys.map(&:to_s) - ["verify"]
missing_needs = expected - needs
fail!("verify is missing needs: #{missing_needs.join(", ")}") unless missing_needs.empty?

changes = jobs["changes"]
fail!("missing changes job") unless changes.is_a?(Hash)
outputs = changes["outputs"].is_a?(Hash) ? changes["outputs"].keys.map(&:to_s) : []
%w[check_needed build_needed test_needed debt_needed full_suite_reason has_tests docs_only].each do |output|
  fail!("changes is missing output #{output}") unless outputs.include?(output)
end

{
  "check" => "check_needed",
  "build" => "build_needed",
  "test" => "test_needed",
  "debt" => "debt_needed",
}.each do |job_name, output|
  job = jobs[job_name]
  fail!("missing #{job_name} job") unless job.is_a?(Hash)
  condition = job["if"].to_s
  fail!("#{job_name} must read changes.outputs.#{output}") unless condition.include?(output)
end

puts "workflow checks passed"
' "$workflow"
