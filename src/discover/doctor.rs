use crate::core::utils::{format_tokens, tool_exists};
use crate::discover::provider::{
    ExtractedCommand, ProviderKind, SelectedProvider, SessionProvider,
};
use crate::discover::registry::{
    classify_command, command_invokes_rtk, extract_ssh_remote_command, split_command_chain,
    Classification,
};
use anyhow::{Context, Result};
use std::collections::HashMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DoctorStatus {
    Ok,
    Warn,
    Error,
}

impl DoctorStatus {
    fn label(self) -> &'static str {
        match self {
            DoctorStatus::Ok => "OK",
            DoctorStatus::Warn => "WARN",
            DoctorStatus::Error => "ERROR",
        }
    }
}

#[derive(Debug, Clone)]
pub struct DoctorSummary {
    pub status: DoctorStatus,
    pub total_commands: usize,
    pub rtk_commands: usize,
    pub supported_raw_commands: usize,
    pub remote_supported_raw_commands: usize,
    pub output_tokens: usize,
    pub findings: Vec<String>,
    top_raw: Vec<(String, usize)>,
}

impl DoctorSummary {
    fn adoption_pct(&self) -> f64 {
        if self.total_commands == 0 {
            0.0
        } else {
            self.rtk_commands as f64 * 100.0 / self.total_commands as f64
        }
    }
}

pub fn analyze_commands(
    provider_kind: ProviderKind,
    commands: &[ExtractedCommand],
) -> DoctorSummary {
    let mut total_commands = 0;
    let mut rtk_commands = 0;
    let mut supported_raw_commands = 0;
    let mut remote_supported_raw_commands = 0;
    let mut raw_counts: HashMap<String, usize> = HashMap::new();

    for ext in commands {
        for part in split_command_chain(&ext.command) {
            total_commands += 1;

            let invokes_rtk = command_invokes_rtk(part);
            if invokes_rtk {
                rtk_commands += 1;
            }

            if let Some(remote) = extract_ssh_remote_command(part) {
                if remote_contains_supported_raw_command(&remote) {
                    supported_raw_commands += 1;
                    remote_supported_raw_commands += 1;
                    *raw_counts
                        .entry("ssh <host> '<supported command>'".to_string())
                        .or_insert(0) += 1;
                }
                continue;
            }

            if !invokes_rtk && matches!(classify_command(part), Classification::Supported { .. }) {
                supported_raw_commands += 1;
                *raw_counts.entry(truncate_command(part)).or_insert(0) += 1;
            }
        }
    }

    let output_tokens: usize = commands
        .iter()
        .filter_map(|cmd| cmd.output_len)
        .sum::<usize>()
        / 4;
    let mut top_raw: Vec<(String, usize)> = raw_counts.into_iter().collect();
    top_raw.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    top_raw.truncate(5);

    let mut findings = Vec::new();
    let status = if total_commands == 0 {
        findings.push(format!(
            "No shell commands were found in the selected {} sessions.",
            provider_kind.label()
        ));
        DoctorStatus::Warn
    } else if rtk_commands == 0 && supported_raw_commands > 0 {
        findings.push(format!(
            "{} found supported shell commands, but none explicitly invoked RTK.",
            provider_kind.label()
        ));
        DoctorStatus::Error
    } else if supported_raw_commands > 0 {
        findings.push(format!(
            "{} supported raw command(s) could have used RTK.",
            supported_raw_commands
        ));
        if remote_supported_raw_commands > 0 {
            findings.push(format!(
                "{} remote SSH command(s) ran supported commands without RTK.",
                remote_supported_raw_commands
            ));
        }
        DoctorStatus::Warn
    } else {
        findings.push("Recent shell commands are RTK-covered or not RTK-supported.".to_string());
        DoctorStatus::Ok
    };

    DoctorSummary {
        status,
        total_commands,
        rtk_commands,
        supported_raw_commands,
        remote_supported_raw_commands,
        output_tokens,
        findings,
        top_raw,
    }
}

pub fn run(
    provider_kind: ProviderKind,
    project: Option<&str>,
    all: bool,
    since_days: u64,
    format: &str,
    verbose: u8,
) -> Result<()> {
    let provider = SelectedProvider::new(provider_kind);
    let project_filter = SelectedProvider::default_project_filter(provider_kind, project, all)?;
    let sessions = provider
        .discover_sessions(project_filter.as_deref(), Some(since_days))
        .with_context(|| format!("Failed to discover {} sessions", provider.label()))?;

    let mut commands = Vec::new();
    let mut parse_errors = 0usize;

    for session in &sessions {
        match provider.extract_commands(session) {
            Ok(mut extracted) => commands.append(&mut extracted),
            Err(err) => {
                parse_errors += 1;
                if verbose > 0 {
                    eprintln!("Warning: failed to parse {}: {err}", session.display());
                }
            }
        }
    }

    let mut summary = analyze_commands(provider_kind, &commands);
    if sessions.is_empty() {
        summary.status = DoctorStatus::Error;
        summary.findings.insert(
            0,
            format!(
                "No {} session files were found in the selected scope.",
                provider.label()
            ),
        );
    }
    if !tool_exists("rtk") {
        summary.status = DoctorStatus::Error;
        summary
            .findings
            .insert(0, "`rtk` was not found on PATH.".to_string());
    }
    if parse_errors > 0 && summary.status == DoctorStatus::Ok {
        summary.status = DoctorStatus::Warn;
    }

    match format {
        "json" => println!(
            "{}",
            format_json(
                provider.label(),
                since_days,
                all,
                sessions.len(),
                parse_errors,
                &summary
            )
        ),
        _ => print!(
            "{}",
            format_text(
                provider.label(),
                since_days,
                all,
                sessions.len(),
                parse_errors,
                &summary
            )
        ),
    }

    Ok(())
}

fn remote_contains_supported_raw_command(remote: &str) -> bool {
    split_command_chain(remote).into_iter().any(|part| {
        !command_invokes_rtk(part)
            && matches!(classify_command(part), Classification::Supported { .. })
    })
}

fn truncate_command(cmd: &str) -> String {
    let trimmed = cmd.trim();
    let parts: Vec<&str> = trimmed.splitn(3, char::is_whitespace).collect();
    match parts.len() {
        0 => String::new(),
        1 => parts[0].to_string(),
        _ => format!("{} {}", parts[0], parts[1]),
    }
}

fn format_text(
    provider_label: &str,
    since_days: u64,
    all: bool,
    sessions_scanned: usize,
    parse_errors: usize,
    summary: &DoctorSummary,
) -> String {
    let scope = if all {
        "all projects"
    } else {
        "current project"
    };
    let mut out = String::new();
    out.push_str(&format!("RTK Doctor - {provider_label}\n"));
    out.push_str("────────────────────────\n");
    out.push_str(&format!("Status: {}\n", summary.status.label()));
    out.push_str(&format!("Scope: {scope}, last {since_days} day(s)\n"));
    out.push_str(&format!("Sessions scanned: {sessions_scanned}\n"));
    out.push_str(&format!("Shell commands: {}\n", summary.total_commands));
    out.push_str(&format!(
        "RTK-covered commands: {} ({:.1}%)\n",
        summary.rtk_commands,
        summary.adoption_pct()
    ));
    out.push_str(&format!(
        "Raw supported commands: {}\n",
        summary.supported_raw_commands
    ));
    if summary.remote_supported_raw_commands > 0 {
        out.push_str(&format!(
            "Raw supported SSH commands: {}\n",
            summary.remote_supported_raw_commands
        ));
    }
    out.push_str(&format!(
        "Observed shell output: ~{} tokens\n",
        format_tokens(summary.output_tokens)
    ));
    if parse_errors > 0 {
        out.push_str(&format!("Parse errors: {parse_errors}\n"));
    }

    out.push_str("\nFindings:\n");
    for finding in &summary.findings {
        out.push_str(&format!("- {finding}\n"));
    }

    if !summary.top_raw.is_empty() {
        out.push_str("\nTop raw commands to fix:\n");
        for (command, count) in &summary.top_raw {
            out.push_str(&format!("- {command} ({count}x)\n"));
        }
    }

    out.push_str("\nNext steps:\n");
    match summary.status {
        DoctorStatus::Ok => {
            out.push_str("- Keep using RTK for high-output shell commands.\n");
        }
        DoctorStatus::Warn | DoctorStatus::Error => {
            out.push_str("- Prefer `rtk <command>` for high-output local commands.\n");
            out.push_str("- For SSH workflows, run RTK inside the remote shell when available.\n");
            out.push_str(
                "- Re-run `rtk doctor --provider codex --all --since 7` after more usage.\n",
            );
        }
    }

    out
}

fn format_json(
    provider_label: &str,
    since_days: u64,
    all: bool,
    sessions_scanned: usize,
    parse_errors: usize,
    summary: &DoctorSummary,
) -> String {
    serde_json::json!({
        "provider": provider_label,
        "since_days": since_days,
        "scope": if all { "all" } else { "current_project" },
        "status": summary.status.label(),
        "sessions_scanned": sessions_scanned,
        "parse_errors": parse_errors,
        "total_commands": summary.total_commands,
        "rtk_commands": summary.rtk_commands,
        "adoption_pct": summary.adoption_pct(),
        "supported_raw_commands": summary.supported_raw_commands,
        "remote_supported_raw_commands": summary.remote_supported_raw_commands,
        "output_tokens_estimate": summary.output_tokens,
        "findings": summary.findings,
        "top_raw": summary.top_raw,
    })
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::discover::provider::{ExtractedCommand, ProviderKind};

    fn cmd(command: &str) -> ExtractedCommand {
        ExtractedCommand {
            command: command.to_string(),
            output_len: Some(400),
            session_id: "session".to_string(),
            output_content: None,
            is_error: false,
            sequence_index: 0,
        }
    }

    #[test]
    fn codex_counts_only_explicit_rtk_as_covered() {
        let summary = analyze_commands(
            ProviderKind::Codex,
            &[cmd("rtk git status"), cmd("git diff"), cmd("echo ok")],
        );

        assert_eq!(summary.total_commands, 3);
        assert_eq!(summary.rtk_commands, 1);
        assert_eq!(summary.supported_raw_commands, 1);
        assert_eq!(summary.status, DoctorStatus::Warn);
    }

    #[test]
    fn codex_flags_supported_remote_ssh_commands_without_rtk() {
        let summary = analyze_commands(
            ProviderKind::Codex,
            &[
                cmd("ssh build-host 'git status'"),
                cmd("ssh build-host 'cd /repo && rtk rg TODO src'"),
            ],
        );

        assert_eq!(summary.total_commands, 2);
        assert_eq!(summary.rtk_commands, 1);
        assert_eq!(summary.remote_supported_raw_commands, 1);
        assert!(summary
            .findings
            .iter()
            .any(|finding| finding.contains("remote")));
    }
}
