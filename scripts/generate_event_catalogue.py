#!/usr/bin/env python3
"""
Generate or verify docs/event-catalogue.md from Rust contract source code.

Scans audit-registry, payment-router, and usage-meter for `#[contractevent]`
definitions and builds the full markdown event catalogue with topics, data layouts,
and emitting entrypoints.
"""

import os
import re
import sys

WORKSPACE_ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))

CONTRACT_INFO = [
    {
        "name": "audit-registry",
        "file": os.path.join(WORKSPACE_ROOT, "audit-registry", "src", "lib.rs"),
        "description": "Attestation registry and caller authorization allowlist.",
        "emissions": {
            "CallerAllowed": "allow_caller(contract)",
            "CallerRevoked": "revoke_caller(contract)",
            "AttestationRecorded": "submit_attestation(caller, subject, id, model_version, policy_ref)",
            "AttestationSuperseded": "supersede_attestation(caller, old_id, new_id)",
        },
    },
    {
        "name": "payment-router",
        "file": os.path.join(WORKSPACE_ROOT, "payment-router", "src", "lib.rs"),
        "description": "Escrow, incremental partial settlements, disputes, and recovery.",
        "emissions": {
            "EscrowOpened": "open_escrow(payer, payee, token, amount)",
            "PartialReleased": "release_partial(id, amount)",
            "Claimed": "claim(id)",
            "Disputed": "dispute(id, amount)",
            "DisputeResolved": "resolve_dispute(id, to_payee)",
            "EscrowClosed": "close_escrow(id)",
            "SettlementFailed": "report_failure(id, reason)",
            "SettlementRecovered": "clear_failure(id)",
            "DeadLettered": "dead_letter(id)",
        },
    },
    {
        "name": "usage-meter",
        "file": os.path.join(WORKSPACE_ROOT, "usage-meter", "src", "lib.rs"),
        "description": "Metered billing units linked to verified attestations.",
        "emissions": {
            "RegistrySet": "set_registry(registry)",
            "PolicySet": "set_unattested_policy(payer, policy)",
            "UsageRecorded": "record_usage(payer, attestation_id, units)",
        },
    },
]


def parse_events(file_path):
    with open(file_path, "r", encoding="utf-8") as f:
        content = f.read()

    # Match #[contractevent] structs
    pattern = re.compile(
        r"#\[contractevent\]\s*(?:#\[[^\]]+\]\s*)*pub struct (\w+)\s*\{([^}]+)\}",
        re.MULTILINE,
    )

    events = []
    for match in pattern.finditer(content):
        name = match.group(1)
        body = match.group(2)

        topics = []
        data = []

        lines = body.strip().split("\n")
        is_topic = False
        for line in lines:
            line = line.strip()
            if not line or line.startswith("//"):
                continue
            if line == "#[topic]":
                is_topic = True
                continue
            field_match = re.match(r"pub\s+(\w+)\s*:\s*([^,]+),?", line)
            if field_match:
                field_name = field_match.group(1)
                field_type = field_match.group(2).strip()
                if is_topic:
                    topics.append((field_name, field_type))
                    is_topic = False
                else:
                    data.append((field_name, field_type))
            else:
                is_topic = False

        events.append({"name": name, "topics": topics, "data": data})

    return events


def build_markdown():
    doc = []
    doc.append("# Contract Event Catalogue & Schema Specification\n")
    doc.append(
        "Events are a formal public interface. Once indexing pipelines, telemetry engines, "
        "or downstream integrations depend on them, altering event schemas without explicit "
        "versioning breaks consumers silently.\n"
    )
    doc.append(
        "Every event emitted by `modeltrace` contracts embeds an explicit schema version in "
        "its topic tuple (`version: u32 = 1`).\n"
    )
    doc.append("## Table of Contents\n")
    doc.append("- [Schema Compatibility & Evolution Policy](#schema-compatibility--evolution-policy)")
    for c in CONTRACT_INFO:
        doc.append(f"- [{c['name']}](#{c['name']})")
    doc.append("- [Automated Catalogue Generation & Verification](#automated-catalogue-generation--verification)\n")

    doc.append("---\n")
    doc.append("## Schema Compatibility & Evolution Policy\n")
    doc.append("### 1. Invariant: Version in Every Event Topic Tuple")
    doc.append(
        "Every event in the workspace defines `#[topic] pub version: u32` as its first topic field. "
        "The resulting Soroban topic vector is always ordered as:"
    )
    doc.append("```")
    doc.append("[0] Event Identifier Symbol (e.g. Symbol::new(&env, \"EscrowOpened\"))")
    doc.append("[1] Schema Version: u32 (current = 1)")
    doc.append("[2..N] Contextual entity identifiers (e.g., id: u64, payer: Address, etc.)")
    doc.append("```")
    doc.append("Indexers can filter topic 1 to immediately route or ignore payloads by version.\n")

    doc.append("### 2. Compatibility Rules")
    doc.append(
        "- **Additive Changes Within a Version**: Adding non-topic data fields with default/optional values "
        "or adding new independent event types is permitted within a schema version."
    )
    doc.append(
        "- **Breaking Changes Require Version Bump**: Any reordering of topics, modification of topic types, "
        "removal of fields, or changes in semantic interpretation is strictly prohibited within the current version. "
        "Such changes require incrementing `EVENT_SCHEMA_VERSION` (e.g. from `1` to `2`)."
    )
    doc.append(
        "- **Dual-Emission During Migration Windows**: When a new schema version is introduced, contracts "
        "must emit both the deprecated version event and the new version event simultaneously during an announced "
        "migration window. This allows downstream consumers to migrate safely without data loss or service disruption."
    )
    doc.append(
        "- **Test Enforcement**: Shape assertion tests (`test_event_shapes_and_versioning`) run in CI to catch "
        "any unintended change at compile/test time.\n"
    )

    doc.append("---\n")

    for c in CONTRACT_INFO:
        doc.append(f"## `{c['name']}`\n")
        doc.append(f"{c['description']}\n")
        events = parse_events(c["file"])
        for ev in events:
            ev_name = ev["name"]
            emission = c["emissions"].get(ev_name, "N/A")
            doc.append(f"### `{ev_name}`\n")
            doc.append(f"- **Emitting Entrypoint**: `{emission}`")
            doc.append(f"- **Topic Tuple**: `(Symbol(\"{ev_name}\"), " + ", ".join([f"{name}: {typ}" for name, typ in ev["topics"]]) + ")`")

            doc.append("\n**Topics:**\n")
            doc.append("| Index | Field | Type | Description |")
            doc.append("|---|---|---|---|")
            doc.append(f"| 0 | *event_name* | `Symbol` | Event identifier (`\"{ev_name}\"`) |")
            for idx, (t_name, t_type) in enumerate(ev["topics"], start=1):
                desc = "Schema version identifier" if t_name == "version" else f"Indexed topic parameter `{t_name}`"
                doc.append(f"| {idx} | `{t_name}` | `{t_type}` | {desc} |")

            doc.append("\n**Data Payload Layout:**\n")
            if ev["data"]:
                doc.append("| Field | Type | Description |")
                doc.append("|---|---|---|")
                for d_name, d_type in ev["data"]:
                    doc.append(f"| `{d_name}` | `{d_type}` | Event data payload field |")
            else:
                doc.append("*None (empty payload / unit data)*\n")
            doc.append("")

        doc.append("---\n")

    doc.append("## Automated Catalogue Generation & Verification\n")
    doc.append(
        "This catalogue is generated from Rust contract source code using `scripts/generate_event_catalogue.py`.\n"
    )
    doc.append("To regenerate or verify the documentation in CI:")
    doc.append("```bash")
    doc.append("python3 scripts/generate_event_catalogue.py --check")
    doc.append("```\n")

    return "\n".join(doc)


def main():
    check_mode = "--check" in sys.argv
    md_content = build_markdown()
    target_path = os.path.join(WORKSPACE_ROOT, "docs", "event-catalogue.md")

    if check_mode:
        if not os.path.exists(target_path):
            print(f"Error: {target_path} does not exist.")
            sys.exit(1)
        with open(target_path, "r", encoding="utf-8") as f:
            existing = f.read()
        if existing.strip() != md_content.strip():
            print("Error: docs/event-catalogue.md is out of sync with contract definitions!")
            sys.exit(1)
        print("Verification passed: docs/event-catalogue.md is up to date.")
    else:
        with open(target_path, "w", encoding="utf-8") as f:
            f.write(md_content)
        print(f"Generated {target_path} successfully.")


if __name__ == "__main__":
    main()
