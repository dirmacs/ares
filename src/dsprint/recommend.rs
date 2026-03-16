use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::LazyLock;

/// Agent recommendation with details
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct AgentRecommendation {
    pub agent_id: String,
    pub agent_name: String,
    pub domain: String,
    pub reason: String,
    pub savings_hours: String,
    pub savings_inr: String,
    pub tier_required: String,
    pub confidence: String,
}

/// Domain score from the request
#[derive(Debug, Serialize, Deserialize)]
pub struct DomainScore {
    pub domain: String,
    pub score: u32,
    pub completeness: f64,
    pub gaps: u32,
}

/// Request for agent recommendations
#[derive(Debug, Serialize, Deserialize)]
pub struct RecommendationRequest {
    pub domain_scores: Vec<DomainScore>,
    pub pain_points: Vec<String>,
    pub team_size: String,
    pub industry: String,
    pub current_tools: Vec<String>,
}

/// Agent catalog entry
struct AgentCatalogEntry {
    id: &'static str,
    name: &'static str,
    domain: &'static str,
    base_hours_min: u32,
    base_hours_max: u32,
    tier: &'static str,
    keywords: &'static [&'static str],
    min_score: u32,
}

/// Full 24-agent catalog using LazyLock
static AGENT_CATALOG: LazyLock<Vec<AgentCatalogEntry>> = LazyLock::new(|| {
    vec![
        // SALES (4)
        AgentCatalogEntry {
            id: "gtm-outreach",
            name: "Outreach Agent",
            domain: "sales",
            base_hours_min: 8,
            base_hours_max: 12,
            tier: "starter",
            keywords: &["outreach", "prospecting", "lead", "cold", "email campaign", "gtm"],
            min_score: 50,
        },
        AgentCatalogEntry {
            id: "gtm-pipeline",
            name: "Pipeline Manager",
            domain: "sales",
            base_hours_min: 5,
            base_hours_max: 10,
            tier: "growth",
            keywords: &["pipeline", "deal", "crm", "sales", "forecast", "opportunity"],
            min_score: 45,
        },
        AgentCatalogEntry {
            id: "lead-qualifier",
            name: "Lead Qualifier",
            domain: "sales",
            base_hours_min: 4,
            base_hours_max: 8,
            tier: "starter",
            keywords: &["qualify", "lead", "score", "filter", "priority", "intent"],
            min_score: 40,
        },
        AgentCatalogEntry {
            id: "crm-updater",
            name: "CRM Updater",
            domain: "sales",
            base_hours_min: 6,
            base_hours_max: 10,
            tier: "starter",
            keywords: &["crm", "update", "tracking", "deals", "manual entry"],
            min_score: 35,
        },
        // MARKETING (4)
        AgentCatalogEntry {
            id: "dinkedin-post-creator",
            name: "LinkedIn Writer",
            domain: "marketing",
            base_hours_min: 5,
            base_hours_max: 8,
            tier: "starter",
            keywords: &["linkedin", "social", "content", "post", "brand"],
            min_score: 40,
        },
        AgentCatalogEntry {
            id: "gtm-strategy",
            name: "Content Strategist",
            domain: "marketing",
            base_hours_min: 8,
            base_hours_max: 15,
            tier: "growth",
            keywords: &["strategy", "content", "plan", "campaign", "marketing"],
            min_score: 55,
        },
        AgentCatalogEntry {
            id: "gtm-analytics",
            name: "Content Analyst",
            domain: "marketing",
            base_hours_min: 3,
            base_hours_max: 6,
            tier: "growth",
            keywords: &["analytics", "metrics", "content", "performance", "roi"],
            min_score: 45,
        },
        AgentCatalogEntry {
            id: "email-drafter",
            name: "Email Drafter",
            domain: "marketing",
            base_hours_min: 4,
            base_hours_max: 7,
            tier: "starter",
            keywords: &["email", "newsletter", "drip", "campaign", "template"],
            min_score: 35,
        },
        // OPERATIONS (4)
        AgentCatalogEntry {
            id: "deployment-monitor",
            name: "Ops Monitor",
            domain: "operations",
            base_hours_min: 10,
            base_hours_max: 15,
            tier: "starter",
            keywords: &["deploy", "monitor", "infrastructure", "ci", "cd", "ops"],
            min_score: 40,
        },
        AgentCatalogEntry {
            id: "meeting-summarizer",
            name: "Meeting Summarizer",
            domain: "operations",
            base_hours_min: 4,
            base_hours_max: 8,
            tier: "starter",
            keywords: &["meeting", "summary", "notes", "agenda", "action"],
            min_score: 30,
        },
        AgentCatalogEntry {
            id: "document-qa",
            name: "Document Q&A",
            domain: "operations",
            base_hours_min: 6,
            base_hours_max: 12,
            tier: "growth",
            keywords: &["document", "search", "qa", "policy", "handbook", "wiki"],
            min_score: 45,
        },
        AgentCatalogEntry {
            id: "incident-reporter",
            name: "Incident Reporter",
            domain: "operations",
            base_hours_min: 3,
            base_hours_max: 6,
            tier: "growth",
            keywords: &["incident", "alert", "monitoring", "downtime", "issue"],
            min_score: 35,
        },
        // CUSTOMER SERVICE (3)
        AgentCatalogEntry {
            id: "ticket-router",
            name: "Ticket Router",
            domain: "customer_service",
            base_hours_min: 8,
            base_hours_max: 15,
            tier: "starter",
            keywords: &["ticket", "routing", "support", "queue", "assign", "customer"],
            min_score: 40,
        },
        AgentCatalogEntry {
            id: "faq-responder",
            name: "FAQ Bot",
            domain: "customer_service",
            base_hours_min: 10,
            base_hours_max: 20,
            tier: "starter",
            keywords: &["faq", "answer", "common", "question", "self-service", "automate"],
            min_score: 35,
        },
        AgentCatalogEntry {
            id: "escalation-handler",
            name: "Escalation Handler",
            domain: "customer_service",
            base_hours_min: 4,
            base_hours_max: 8,
            tier: "growth",
            keywords: &["escalation", "priority", "urgent", "resolve", "sla"],
            min_score: 40,
        },
        // FINANCE (3)
        AgentCatalogEntry {
            id: "invoice-processor",
            name: "Invoice Processor",
            domain: "finance",
            base_hours_min: 6,
            base_hours_max: 10,
            tier: "growth",
            keywords: &["invoice", "billing", "payment", "vendor", "expense", "ap"],
            min_score: 40,
        },
        AgentCatalogEntry {
            id: "finance-analyst",
            name: "Finance Analyst",
            domain: "finance",
            base_hours_min: 5,
            base_hours_max: 10,
            tier: "growth",
            keywords: &["financial", "analysis", "report", "kpi", "metrics", "budget"],
            min_score: 45,
        },
        AgentCatalogEntry {
            id: "report-generator",
            name: "Report Generator",
            domain: "finance",
            base_hours_min: 4,
            base_hours_max: 8,
            tier: "starter",
            keywords: &["report", "dashboard", "data", "visualization", "summary"],
            min_score: 35,
        },
        // VERTICAL (6)
        AgentCatalogEntry {
            id: "lexflow-contract-analyzer",
            name: "Contract Analyzer",
            domain: "legal",
            base_hours_min: 8,
            base_hours_max: 15,
            tier: "pro",
            keywords: &["contract", "legal", "clause", "review", "agreement"],
            min_score: 50,
        },
        AgentCatalogEntry {
            id: "lexflow-compliance-checker",
            name: "Compliance Checker",
            domain: "legal",
            base_hours_min: 6,
            base_hours_max: 12,
            tier: "pro",
            keywords: &["compliance", "regulation", "audit", "legal", "policy"],
            min_score: 45,
        },
        AgentCatalogEntry {
            id: "talentloop-resume-screener",
            name: "Resume Screener",
            domain: "hr",
            base_hours_min: 5,
            base_hours_max: 10,
            tier: "growth",
            keywords: &["resume", "hiring", "screening", "candidate", "recruit"],
            min_score: 40,
        },
        AgentCatalogEntry {
            id: "talentloop-onboarding-assistant",
            name: "Onboarding Assistant",
            domain: "hr",
            base_hours_min: 4,
            base_hours_max: 8,
            tier: "growth",
            keywords: &["onboarding", "new hire", "training", "orientation"],
            min_score: 35,
        },
        AgentCatalogEntry {
            id: "clearledger-ap-automation",
            name: "AP Automation",
            domain: "finance",
            base_hours_min: 8,
            base_hours_max: 15,
            tier: "pro",
            keywords: &["accounts payable", "ap", "payment", "vendor", "invoice"],
            min_score: 50,
        },
        AgentCatalogEntry {
            id: "clearledger-transaction-anomaly",
            name: "Transaction Monitor",
            domain: "finance",
            base_hours_min: 3,
            base_hours_max: 6,
            tier: "pro",
            keywords: &["transaction", "anomaly", "fraud", "monitoring", "financial"],
            min_score: 40,
        },
    ]
});

/// Team multiplier: 1-5: 0.5, 6-15: 0.8, 16-50: 1.0, 51-200: 1.5, 200+: 2.0
fn team_size_multiplier(team_size: &str) -> f64 {
    let size = parse_team_size(team_size);
    match size {
        1..=5 => 0.5,
        6..=15 => 0.8,
        16..=50 => 1.0,
        51..=200 => 1.5,
        _ => 2.0,
    }
}

/// Hourly rate: 1-5: 350, 6-50: 500, 51+: 700
fn hourly_rate(team_size: &str) -> f64 {
    let size = parse_team_size(team_size);
    match size {
        1..=5 => 350.0,
        6..=50 => 500.0,
        _ => 700.0,
    }
}

/// Parse team size string to numeric estimate
fn parse_team_size(team_size: &str) -> u32 {
    // Extract first number from strings like "16-50 people", "1-10", "51+", etc.
    let cleaned = team_size
        .chars()
        .filter(|c| c.is_digit(10) || *c == '-')
        .collect::<String>();
    
    if let Some(dash_pos) = cleaned.find('-') {
        let start = &cleaned[..dash_pos];
        start.parse::<u32>().unwrap_or(0)
    } else if let Some(plus_pos) = cleaned.find('+') {
        let num = &cleaned[..plus_pos];
        num.parse::<u32>().unwrap_or(0)
    } else {
        cleaned.parse::<u32>().unwrap_or(0)
    }
}

/// Tier recommendation logic
fn compute_tier(num_agents: usize, team_size: &str) -> (String, String) {
    let size = parse_team_size(team_size);
    let (tier, price) = match num_agents {
        0 => ("free".to_string(), "Rs.0/mo".to_string()),
        1..=3 => ("starter".to_string(), "Rs.5,000/mo".to_string()),
        4..=8 => ("growth".to_string(), "Rs.15,000/mo".to_string()),
        9..=15 => ("pro".to_string(), "Rs.35,000/mo".to_string()),
        _ => ("enterprise".to_string(), "Custom pricing".to_string()),
    };
    
    // Adjust for large teams
    if size > 200 && tier == "starter" {
        ("enterprise".to_string(), "Custom pricing".to_string())
    } else if size > 50 && tier == "starter" {
        ("growth".to_string(), "Rs.15,000/mo".to_string())
    } else {
        (tier, price)
    }
}

/// Main recommendation function
pub fn recommend_agents(req: &RecommendationRequest) -> Vec<AgentRecommendation> {
    let mut recommendations = Vec::new();

    // Find domain score for the current request's domains
    for agent in AGENT_CATALOG.iter() {
        // Find matching domain score
        let domain_score_opt = req
            .domain_scores
            .iter()
            .find(|ds| ds.domain == agent.domain);

        let domain_score = match domain_score_opt {
            Some(ds) => ds.score,
            None => continue, // Skip if no score for this domain
        };

        // Skip if score below minimum
        if domain_score < agent.min_score {
            continue;
        }

        // Check keyword match against pain points
        let has_keyword_match = req.pain_points.iter().any(|pain_point| {
            agent.keywords.iter().any(|&keyword| {
                pain_point
                    .to_lowercase()
                    .contains(&keyword.to_lowercase())
            })
        });

        // Skip if no keyword match AND score < 60
        if !has_keyword_match && domain_score < 60 {
            continue;
        }

        // Generate personalized reason
        let reason = generate_reason(agent, req, domain_score, has_keyword_match);

        // Estimate savings with team multiplier and hourly rate
        let (savings_hours, savings_inr) = estimate_savings(agent, &req.team_size);

        // Calculate confidence based on domain score and keyword match
        let confidence_score = if has_keyword_match {
            domain_score + 20
        } else {
            domain_score
        };
        let confidence = format!("{}%", confidence_score.min(100));

        recommendations.push(AgentRecommendation {
            agent_id: agent.id.to_string(),
            agent_name: agent.name.to_string(),
            domain: agent.domain.to_string(),
            reason,
            savings_hours,
            savings_inr,
            tier_required: agent.tier.to_string(),
            confidence,
        });
    }

    // Sort by confidence (descending) - extract numeric value for sorting
    recommendations.sort_by(|a, b| {
        let a_conf = a.confidence.trim_end_matches('%').parse::<u32>().unwrap_or(0);
        let b_conf = b.confidence.trim_end_matches('%').parse::<u32>().unwrap_or(0);
        b_conf.cmp(&a_conf)
    });

    // Truncate to top 8
    recommendations.truncate(8);

    recommendations
}

/// Generate personalized reason for recommendation
fn generate_reason(
    agent: &AgentCatalogEntry,
    req: &RecommendationRequest,
    domain_score: u32,
    has_keyword_match: bool,
) -> String {
    let mut reason = String::new();

    // Start with domain-specific context
    reason.push_str(&format!(
        "Your {} domain scores {} out of 100. ",
        agent.domain, domain_score
    ));

    if has_keyword_match {
        reason.push_str("Your pain points directly match this agent's capabilities. ");
    } else {
        reason.push_str("This agent aligns with your domain strengths. ");
    }

    // Add team size context
    let size = parse_team_size(&req.team_size);
    match size {
        1..=10 => reason.push_str("Suitable for small teams."),
        11..=50 => reason.push_str("Optimized for growing teams."),
        51..=200 => reason.push_str("Scalable for mid-size organizations."),
        _ => reason.push_str("Enterprise-grade solution."),
    }

    reason
}

/// Estimate hours saved per week and savings in INR per month
fn estimate_savings(agent: &AgentCatalogEntry, team_size: &str) -> (String, String) {
    let multiplier = team_size_multiplier(team_size);
    let rate = hourly_rate(team_size);
    
    // Calculate adjusted hours
    let adjusted_min = (agent.base_hours_min as f64 * multiplier).round() as u32;
    let adjusted_max = (agent.base_hours_max as f64 * multiplier).round() as u32;
    let savings_hours = format!("{}-{}", adjusted_min, adjusted_max);
    
    // Calculate INR savings (monthly: weekly hours * 4.33 weeks * rate)
    let monthly_min = (adjusted_min as f64 * 4.33 * rate).round() as u64;
    let monthly_max = (adjusted_max as f64 * 4.33 * rate).round() as u64;
    let savings_inr = format!("₹{}-₹{}", monthly_min, monthly_max);
    
    (savings_hours, savings_inr)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_team_size() {
        assert_eq!(parse_team_size("1-10"), 1);
        assert_eq!(parse_team_size("11-50"), 11);
        assert_eq!(parse_team_size("51-200"), 51);
        assert_eq!(parse_team_size("201+"), 201);
        assert_eq!(parse_team_size("16-50 people"), 16);
        assert_eq!(parse_team_size("unknown"), 0);
    }

    #[test]
    fn test_team_size_multiplier() {
        assert_eq!(team_size_multiplier("1-10"), 0.5);
        assert_eq!(team_size_multiplier("6-15"), 0.8);
        assert_eq!(team_size_multiplier("16-50"), 1.0);
        assert_eq!(team_size_multiplier("51-200"), 1.5);
        assert_eq!(team_size_multiplier("201+"), 2.0);
        assert_eq!(team_size_multiplier("unknown"), 0.5);
    }

    #[test]
    fn test_hourly_rate() {
        assert_eq!(hourly_rate("1-5"), 350.0);
        assert_eq!(hourly_rate("6-50"), 500.0);
        assert_eq!(hourly_rate("51+"), 700.0);
        assert_eq!(hourly_rate("unknown"), 350.0);
    }

    #[test]
    fn test_compute_tier() {
        assert_eq!(compute_tier(0, "16-50"), ("free".to_string(), "Rs.0/mo".to_string()));
        assert_eq!(compute_tier(2, "16-50"), ("starter".to_string(), "Rs.5,000/mo".to_string()));
        assert_eq!(compute_tier(5, "16-50"), ("growth".to_string(), "Rs.15,000/mo".to_string()));
        assert_eq!(compute_tier(12, "16-50"), ("pro".to_string(), "Rs.35,000/mo".to_string()));
        assert_eq!(compute_tier(20, "16-50"), ("enterprise".to_string(), "Custom pricing".to_string()));
    }

    #[test]
    fn test_estimate_savings() {
        let agent = AgentCatalogEntry {
            id: "test",
            name: "Test Agent",
            domain: "sales",
            base_hours_min: 10,
            base_hours_max: 20,
            tier: "starter",
            keywords: &["test"],
            min_score: 30,
        };
        
        let (hours, inr) = estimate_savings(&agent, "16-50");
        assert_eq!(hours, "10-20"); // multiplier 1.0
        assert!(inr.starts_with("₹"));
        
        let (hours_small, inr_small) = estimate_savings(&agent, "1-5");
        assert_eq!(hours_small, "5-10"); // multiplier 0.5: 10*0.5=5, 20*0.5=10
    }

    #[test]
    fn test_recommend_agents_basic() {
        let req = RecommendationRequest {
            domain_scores: vec![
                DomainScore {
                    domain: "sales".to_string(),
                    score: 75,
                    completeness: 0.8,
                    gaps: 3,
                },
                DomainScore {
                    domain: "marketing".to_string(),
                    score: 60,
                    completeness: 0.7,
                    gaps: 5,
                },
            ],
            pain_points: vec![
                "Need to improve outreach to prospects".to_string(),
                "Pipeline management is manual".to_string(),
            ],
            team_size: "11-50".to_string(),
            industry: "Technology".to_string(),
            current_tools: vec!["CRM".to_string()],
        };

        let recs = recommend_agents(&req);
        assert!(!recs.is_empty());

        // Should include sales agents due to high score and keyword match
        let has_outreach = recs.iter().any(|r| r.agent_id == "gtm-outreach");
        let has_pipeline = recs.iter().any(|r| r.agent_id == "gtm-pipeline");
        assert!(has_outreach || has_pipeline);

        // All recommendations should have confidence > 0
        for rec in &recs {
            assert!(rec.confidence.ends_with('%'));
            let conf = rec.confidence.trim_end_matches('%').parse::<u32>().unwrap();
            assert!(conf > 0);
        }
    }

    #[test]
    fn test_recommend_agents_low_score_no_keywords() {
        let req = RecommendationRequest {
            domain_scores: vec![DomainScore {
                domain: "operations".to_string(),
                score: 55, // Below 60
                completeness: 0.6,
                gaps: 8,
            }],
            pain_points: vec![
                "Need better documentation".to_string(), // Not matching keywords
            ],
            team_size: "1-10".to_string(),
            industry: "Finance".to_string(),
            current_tools: vec![],
        };

        let recs = recommend_agents(&req);
        // deployment-monitor and meeting-summarizer have min_score 40 and 30 respectively, but no keyword match
        // Since score < 60 and no keyword match, they should be skipped
        assert!(recs.is_empty());
    }

    #[test]
    fn test_recommend_agents_high_score_no_keywords() {
        let req = RecommendationRequest {
            domain_scores: vec![DomainScore {
                domain: "operations".to_string(),
                score: 75, // Above 60
                completeness: 0.8,
                gaps: 4,
            }],
            pain_points: vec![
                "Need better documentation".to_string(), // Not matching keywords
            ],
            team_size: "1-10".to_string(),
            industry: "Finance".to_string(),
            current_tools: vec![],
        };

        let recs = recommend_agents(&req);
        // Should include document-qa (min_score 45, score 75 > 60, so no keyword match needed)
        let has_doc_qa = recs.iter().any(|r| r.agent_id == "document-qa");
        assert!(has_doc_qa);
    }

    #[test]
    fn test_recommend_agents_construction_company() {
        let req = RecommendationRequest {
            domain_scores: vec![
                DomainScore { domain: "operations".to_string(), score: 82, completeness: 0.78, gaps: 3 },
                DomainScore { domain: "sales".to_string(), score: 55, completeness: 0.5, gaps: 5 },
                DomainScore { domain: "finance".to_string(), score: 45, completeness: 0.4, gaps: 4 },
            ],
            pain_points: vec![
                "Manual data entry between systems".to_string(),
                "Searching for information across sites".to_string(),
                "Tracking costs across projects".to_string(),
            ],
            team_size: "16-50 people".to_string(),
            industry: "Construction".to_string(),
            current_tools: vec![],
        };
        let recs = recommend_agents(&req);
        assert!(!recs.is_empty());
        // Should include ops agents due to high operations score
        let has_ops = recs.iter().any(|r| r.domain == "operations");
        assert!(has_ops);
    }
}
