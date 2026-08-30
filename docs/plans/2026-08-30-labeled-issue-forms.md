# Labeled Issue Forms Implementation Plan

> **For agentic workers:** use the `subagent-driven-development` skill to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Route normal GitHub issue creation through bug, feature, or refactor forms that automatically apply the matching label.

**Architecture:** Use native GitHub issue forms under `.github/ISSUE_TEMPLATE/`; no GitHub Action or dependency. Disable blank issues so GitHub's web UI always starts from a labeled form. This does not prevent maintainers or integrations from creating unlabeled issues through CLI/API.

**Tech Stack:** GitHub issue forms (YAML), GitHub CLI.

## Global Constraints

- Reuse GitHub's existing `bug` and `enhancement` labels.
- Add one custom label: `refactor`.
- Use `enhancement` for user-facing features and `refactor` for behavior-preserving internal changes.
- Request sanitized diagnostics only; never ask reporters to publish credentials, prompts, transcripts, usernames, home paths, hostnames, or private repository details.
- Do not add a workflow to police labels after issue creation.

---

### Task 1: Add labeled issue forms

**Files:**

- Create: `.github/ISSUE_TEMPLATE/config.yml`
- Create: `.github/ISSUE_TEMPLATE/bug.yml`
- Create: `.github/ISSUE_TEMPLATE/feature.yml`
- Create: `.github/ISSUE_TEMPLATE/refactor.yml`

**Interfaces:**

- Consumes: GitHub's issue-form schema and repository labels.
- Produces: three issue-creation choices that apply `bug`, `enhancement`, or `refactor`.

- [ ] **Step 1: Create `.github/ISSUE_TEMPLATE/config.yml`**

```yaml
blank_issues_enabled: false
contact_links: []
```

- [ ] **Step 2: Create `.github/ISSUE_TEMPLATE/bug.yml`**

```yaml
name: Bug report
description: Report broken or unexpected behavior
title: ""
labels:
  - bug
body:
  - type: markdown
    attributes:
      value: Thanks for reporting a bug. Remove private data from captures and logs before submitting.
  - type: textarea
    id: happened
    attributes:
      label: What happened?
      description: Describe observed behavior.
    validations:
      required: true
  - type: textarea
    id: expected
    attributes:
      label: What did you expect?
    validations:
      required: true
  - type: textarea
    id: reproduce
    attributes:
      label: Steps to reproduce
      placeholder: |
        1. ...
        2. ...
        3. ...
    validations:
      required: true
  - type: textarea
    id: environment
    attributes:
      label: Environment
      description: Include AgentsMon version, OS, tmux version, and agent type.
    validations:
      required: true
  - type: textarea
    id: context
    attributes:
      label: Additional context
      description: Add sanitized logs or pane captures when relevant. Never include credentials, prompts, transcripts, or private identifiers.
```

- [ ] **Step 3: Create `.github/ISSUE_TEMPLATE/feature.yml`**

```yaml
name: Feature request
description: Propose new user-facing behavior
title: ""
labels:
  - enhancement
body:
  - type: textarea
    id: problem
    attributes:
      label: What problem should this solve?
    validations:
      required: true
  - type: textarea
    id: proposal
    attributes:
      label: Proposed behavior
    validations:
      required: true
  - type: textarea
    id: alternatives
    attributes:
      label: Alternatives considered
      description: Optional; include simpler workarounds if any.
```

- [ ] **Step 4: Create `.github/ISSUE_TEMPLATE/refactor.yml`**

```yaml
name: Refactor
description: Propose an internal change that preserves behavior
title: ""
labels:
  - refactor
body:
  - type: textarea
    id: problem
    attributes:
      label: What makes the current design hard to change?
    validations:
      required: true
  - type: textarea
    id: scope
    attributes:
      label: Proposed scope
      description: Name responsibilities or boundaries to change; avoid prescribing speculative abstractions.
    validations:
      required: true
  - type: textarea
    id: preserved
    attributes:
      label: What behavior must remain unchanged?
    validations:
      required: true
  - type: textarea
    id: verification
    attributes:
      label: Verification
      description: List tests and manual checks that should prove behavior stayed intact.
    validations:
      required: true
```

- [ ] **Step 5: Parse every YAML file**

Run:

```bash
ruby -e 'require "yaml"; Dir[".github/ISSUE_TEMPLATE/*.yml"].each { |f| YAML.load_file(f); puts "ok #{f}" }'
```

Expected: one `ok` line per file and exit status 0.

- [ ] **Step 6: Commit repository configuration**

```bash
git add .github/ISSUE_TEMPLATE
git commit -m "chore: add labeled issue forms"
```

### Task 2: Add and apply the refactor label

**Files:**

- Modify: GitHub repository label configuration
- Modify: GitHub issue #21 labels

**Interfaces:**

- Consumes: `refactor` label referenced by `.github/ISSUE_TEMPLATE/refactor.yml`.
- Produces: working auto-label target and accurate classification for existing refactor issue #21.

- [ ] **Step 1: Create the label idempotently**

```bash
gh label create refactor \
  --repo snirt/tmux-agents-mon \
  --color D4C5F9 \
  --description "Internal change that preserves behavior" \
  --force
```

Expected: command exits 0 and `refactor` appears in `gh label list`.

- [ ] **Step 2: Reclassify issue #21**

```bash
gh issue edit 21 \
  --repo snirt/tmux-agents-mon \
  --remove-label enhancement \
  --add-label refactor
```

- [ ] **Step 3: Verify labels**

```bash
gh issue view 21 --repo snirt/tmux-agents-mon \
  --json labels --jq '[.labels[].name] | sort | join(", ")'
```

Expected: `refactor` and not `enhancement`.

- [ ] **Step 4: Verify forms after pushing**

Open `https://github.com/snirt/tmux-agents-mon/issues/new/choose` and confirm:

- Bug report, Feature request, and Refactor choices appear.
- No blank-issue choice appears.
- Opening each form shows its expected label.
- Bug form warns reporters to sanitize diagnostics.
