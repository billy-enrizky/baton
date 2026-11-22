#!/usr/bin/env bash
set -euo pipefail

workflow=
base_workflow=
allow_change=false
bootstrap=false

while [[ $# -gt 0 ]]; do
  case "$1" in
    --base)
      base_workflow=${2:?--base requires a workflow}
      shift 2
      ;;
    --allow-change)
      allow_change=true
      shift
      ;;
    --bootstrap)
      bootstrap=true
      shift
      ;;
    *)
      workflow=$1
      shift
      ;;
  esac
done

if [[ -z "$workflow" ]]; then
  echo "usage: ci/check-ci.sh [--base <base-workflow>] [--allow-change] [--bootstrap] <workflow>" >&2
  exit 2
fi

ruby -ryaml -e '
def fail!(rule, message)
  warn "ERROR: #{rule} #{message}"
  exit 1
end

def events(document)
  on = document["on"] || document[true]
  on.is_a?(Hash) ? on.keys.map(&:to_s) : Array(on).map(&:to_s)
end

def canonical_steps(job)
  Array(job["steps"]).map do |step|
    {
      "uses" => step["uses"],
      "run" => step["run"],
      "with" => step["with"],
      "if" => step["if"],
      "continue-on-error" => step["continue-on-error"],
      "env" => step["env"],
      "shell" => step["shell"],
      "working-directory" => step["working-directory"],
    }
  end
end

def canonical_job(job)
  {
    "needs" => job["needs"],
    "if" => job["if"],
    "timeout-minutes" => job["timeout-minutes"],
    "runs-on" => job["runs-on"],
    "strategy" => job["strategy"],
    "outputs" => job["outputs"],
    "with" => job["with"],
    "secrets" => job["secrets"],
    "steps" => canonical_steps(job),
  }
end

head_path, base_path, allow_change, bootstrap = ARGV
allow_change = allow_change == "true"
bootstrap = bootstrap == "true"
head = YAML.load_file(head_path)
jobs = head["jobs"]
fail!("CI001", "workflow has no jobs") unless jobs.is_a?(Hash)

trigger_events = events(head)
%w[pull_request push merge_group workflow_dispatch].each do |event|
  fail!("CI001", "missing trigger #{event}") unless trigger_events.include?(event)
end
on = head["on"] || head[true]
push = on.is_a?(Hash) ? on["push"] : nil
branches = push.is_a?(Hash) ? Array(push["branches"]).map(&:to_s) : []
fail!("CI001", "push trigger must target main") unless branches.include?("main")

fail!("CI015", "top-level env is not allowed; a head-controlled env can shadow cargo on PATH") if head.key?("env")
fail!("CI015", "top-level defaults is not allowed") if head.key?("defaults")

jobs.each do |name, job|
  fail!("CI002", "job #{name} has no timeout-minutes") unless job.key?("timeout-minutes")
  fail!("CI015", "job #{name} sets env; a head-controlled env can shadow cargo on PATH") if job.key?("env")
  fail!("CI015", "job #{name} sets defaults") if job.key?("defaults")
  fail!("CI015", "job #{name} sets container") if job.key?("container")
  fail!("CI015", "job #{name} sets services") if job.key?("services")
  fail!("CI015", "job #{name} runs a reusable workflow") if job.key?("uses")
  fail!("CI006", "job #{name} sets permissions") if job.key?("permissions")
  continue_on_error = job["continue-on-error"]
  if job.key?("continue-on-error") && continue_on_error.to_s != "false"
    fail!("CI003", "job #{name} sets continue-on-error")
  end
  fail!("CI002", "job #{name} has no runs-on") unless job.key?("runs-on")
  timeout = job["timeout-minutes"]
  fail!("CI002", "job #{name} timeout must be numeric and below 360") unless timeout.is_a?(Numeric) && timeout.positive? && timeout < 360

  steps = Array(job["steps"])
  steps.each do |step|
    uses = step["uses"].to_s
    fail!("CI003", "job #{name} uses a retry action") if uses.match?(/retry/i)
    if step.key?("continue-on-error") && step["continue-on-error"].to_s != "false"
      fail!("CI003", "job #{name} uses continue-on-error")
    end
    fail!("CI015", "job #{name} sets a step env") if step.key?("env")
    fail!("CI015", "job #{name} sets a step shell") if step.key?("shell")
    fail!("CI015", "job #{name} sets a step working-directory") if step.key?("working-directory")
    run = step["run"].to_s
    fail!("CI004", "job #{name} masks a shell failure with || true") if run.include?("|| true")
    fail!("CI004", "job #{name} masks a shell failure with set +e") if run.include?("set +e") || run.include?("set +o errexit")

    if !uses.empty? && !uses.start_with?("./") && !uses.match?(/@[0-9a-f]{40}\z/)
      fail!("CI011", "job #{name} action is not pinned by a 40-character SHA: #{uses}")
    end

    if !run.empty? && step.key?("if") && run.match?(/cargo (check|build|test)|budget\.sh|fmt-debt\.sh|clippy-debt\.sh/)
      fail!("CI012", "required command step in #{name} has a step-level if")
    end
  end
end

fail!("CI006", "workflow permissions must be contents: read") unless head.dig("permissions", "contents") == "read"
permissions = head["permissions"].is_a?(Hash) ? head["permissions"] : {}
permissions.each do |name, value|
  fail!("CI006", "permission #{name} must be read") unless value.to_s == "read"
end
fail!("CI008", "pull_request_target is not allowed") if trigger_events.include?("pull_request_target")

group = head.dig("concurrency", "group").to_s
cancel = head.dig("concurrency", "cancel-in-progress").to_s
fail!("CI007", "concurrency must key by PR number or commit") unless group.include?("pull_request.number") && group.include?("github.sha")
fail!("CI007", "pull requests must cancel superseded runs") unless cancel.include?("pull_request")

verify = jobs["verify"]
fail!("CI005", "missing verify job") unless verify.is_a?(Hash)
fail!("CI005", "verify must use if: always()") unless verify["if"].to_s.include?("always()")
needs = Array(verify["needs"]).map(&:to_s)
expected = jobs.keys.map(&:to_s) - ["verify"]
missing_needs = expected - needs
fail!("CI005", "verify is missing needs: #{missing_needs.join(", ")}") unless missing_needs.empty?

changes = jobs["changes"]
fail!("CI009", "missing changes job") unless changes.is_a?(Hash)
outputs = changes["outputs"].is_a?(Hash) ? changes["outputs"].keys.map(&:to_s) : []
%w[check_needed build_needed test_needed debt_needed full_suite_reason has_tests docs_only].each do |output|
  fail!("CI009", "changes is missing output #{output}") unless outputs.include?(output)
end

{
  "check" => "check_needed",
  "build" => "build_needed",
  "test" => "test_needed",
  "debt" => "debt_needed",
}.each do |job_name, output|
  job = jobs[job_name]
  fail!("CI010", "missing #{job_name} job") unless job.is_a?(Hash)
  condition = job["if"].to_s
  fail!("CI010", "#{job_name} must read changes.outputs.#{output}") unless condition.include?(output)
end

unless allow_change
  required_commands = {
    "changes" => ["classify-paths.sh", "selftest.sh", "check-ci.sh"],
    "selection" => ["classify-paths.sh", "check-ci.sh", "--base"],
    "check" => ["cargo check --locked"],
    "build" => ["cargo build --locked"],
    "test" => ["cargo test --locked"],
    "debt" => ["fmt-debt.sh", "clippy-debt.sh"],
    "verify" => ["check_gate"],
  }
  job_text = {}
  required_commands.each do |job_name, commands|
    job = jobs[job_name]
    fail!("CI014", "missing #{job_name} job") unless job.is_a?(Hash)
    text = Array(job["steps"]).map { |step| step["run"].to_s }.join("\n")
    job_text[job_name] = text
    commands.each do |command|
      fail!("CI014", "#{job_name} is missing required command #{command}") unless text.include?(command)
    end
  end
  required_commands.each do |job_name, commands|
    text = job_text[job_name]
    commands.each do |command|
      next if command == "check_gate" || command.start_with?("--")
      budgeted = Regexp.new("budget\\.sh\"?\\s+\\d+\\s+\\S*" + Regexp.escape(command))
      fail!("CI017", "#{job_name} runs #{command} outside ci/budget.sh") unless text.match?(budgeted)
    end
  end

end

if base_path.to_s.empty? || !File.file?(base_path)
  fail!("CI013", "base workflow is missing and --bootstrap was not set") unless bootstrap
  puts "CI013 skipped: bootstrap workflow"
elsif File.file?(base_path)
  base = YAML.load_file(base_path)
  base_jobs = base["jobs"]
  fail!("CI013", "base workflow has no jobs") unless base_jobs.is_a?(Hash)

  head_jobs = jobs.keys.map(&:to_s).sort
  base_keys = base_jobs.keys.map(&:to_s).sort
  fail!("CI013", "job list changed: #{head_jobs} != #{base_keys}") unless head_jobs == base_keys

  changed = []
  head_jobs.each do |name|
    next if canonical_job(jobs[name]) == canonical_job(base_jobs[name])
    changed << name
  end
  if !changed.empty? && !allow_change
    fail!("CI013", "workflow commands, needs, if, timeout, or runner changed without --allow-change: #{changed.join(", ")}")
  elsif !changed.empty?
    puts "CI013 allowed by local --allow-change: #{changed.join(", ")}"
  end
end

puts "workflow checks passed"
' "$workflow" "$base_workflow" "$allow_change" "$bootstrap"
