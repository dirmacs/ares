//! Email templates for DSprint survey lifecycle.

/// Results email — sent immediately after survey completion.
pub fn results_email(company: &str, overall_score: u32, recommendations: &[String], workspace_id: &str) -> (String, String) {
    let recs = if recommendations.is_empty() {
        "We're analyzing your business to find the best agents.".to_string()
    } else {
        recommendations.iter().enumerate()
            .map(|(i, r)| format!("  {}. {}", i + 1, r))
            .collect::<Vec<_>>().join("\n")
    };
    let subject = format!("Your DSprint Results — {} scores {}/100", company, overall_score);
    let body = format!(r#"Hi there,

Your DSprint analysis for {} is complete!

Overall Score: {}/100

Top Recommendations:
{}

View full results: https://dsprint.dirmacs.com/results/{}

— The DIRMACS Team"#, company, overall_score, recs, workspace_id);
    (subject, body)
}

/// Follow-up email (day 2) — improve recommendations.
pub fn followup_email(company: &str, gaps_count: u32) -> (String, String) {
    let subject = format!("3 quick questions to improve your {} recommendations", company);
    let body = format!(r#"Hi,

We identified {} areas where a few more details could significantly improve your AI agent recommendations for {}.

It takes less than 2 minutes:
https://dsprint.dirmacs.com/continue

The more we know about your workflow, the better we can match you with the right agents.

— The DIRMACS Team"#, gaps_count, company);
    (subject, body)
}

/// MCP intro email (day 5) — connect AI tools.
pub fn mcp_intro_email(company: &str) -> (String, String) {
    let subject = "Connect your AI tools to DIRMACS via MCP".to_string();
    let body = format!(r#"Hi,

Your {} agents are ready. Want to use them directly from Claude, ChatGPT, or Cursor?

DIRMACS supports the Model Context Protocol (MCP). One connection, all your agents:

  MCP Endpoint: eruka.dirmacs.com/mcp
  Setup Guide: https://docs.dirmacs.com/mcp-setup

It takes 2 minutes to connect. Your agents will be available right in your AI assistant.

— The DIRMACS Team"#, company);
    (subject, body)
}

/// Engagement email (day 14) — activation nudge.
pub fn activation_nudge_email(company: &str, workspace_id: &str) -> (String, String) {
    let subject = format!("Your {} agents are ready — activate now", company);
    let body = format!(r#"Hi,

You completed the DSprint survey for {} but haven't activated yet.

Your recommended agents are ready to start saving you time. Activate now:
https://dsprint.dirmacs.com/results/{}/activate

Plans start at Rs.5,000/mo for up to 3 agents.

— The DIRMACS Team"#, company, workspace_id);
    (subject, body)
}
