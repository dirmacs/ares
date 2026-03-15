use serde::{Deserialize, Serialize};

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
    base_hours: &'static str,
    tier: &'static str,
    keywords: &'static [&'static str],
    min_score: u32,
}

/// Static agent catalog with 12 agents across 5 domains
static AGENT_CATALOG: &[AgentCatalogEntry] = &[
    // Sales domain
    AgentCatalogEntry {
        id: "gtm-outreach",
        name: "Go-To-Market Outreach Agent",
        domain: "sales",
        base_hours: "20-30",
        tier: "growth",
        keywords: &["outreach", "prospecting", "lead", "contact", "email", "campaign", "gtm"],
        min_score: 50,
    },
    AgentCatalogEntry {
        id: "gtm-pipeline",
        name: "Pipeline Management Agent",
        domain: "sales",
        base_hours: "15-25",
        tier: "growth",
        keywords: &["pipeline", "deal", "opportunity", "crm", "sales", "forecast", "stage"],
        min_score: 45,
    },
    AgentCatalogEntry {
        id: "lead-qualifier",
        name: "Lead Qualification Agent",
        domain: "sales",
        base_hours: "10-20",
        tier: "starter",
        keywords: &["qualify", "lead", "score", "filter", "priority", "fit", "intent"],
        min_score: 40,
    },
    // Marketing domain
    AgentCatalogEntry {
        id: "linkedin-post-creator",
        name: "LinkedIn Content Creator",
        domain: "marketing",
        base_hours: "12-18",
        tier: "starter",
        keywords: &["linkedin", "social", "post", "content", "brand", "engagement", "marketing"],
        min_score: 40,
    },
    AgentCatalogEntry {
        id: "gtm-strategy",
        name: "GTM Strategy Agent",
        domain: "marketing",
        base_hours: "25-35",
        tier: "growth",
        keywords: &["strategy", "gtm", "market", "positioning", "campaign", "launch", "plan"],
        min_score: 55,
    },
    // Operations domain
    AgentCatalogEntry {
        id: "deployment-monitor",
        name: "Deployment Monitor",
        domain: "operations",
        base_hours: "8-15",
        tier: "starter",
        keywords: &["deploy", "deployment", "monitor", "ci", "cd", "release", "infrastructure"],
        min_score: 35,
    },
    AgentCatalogEntry {
        id: "meeting-summarizer",
        name: "Meeting Summarizer",
        domain: "operations",
        base_hours: "5-10",
        tier: "starter",
        keywords: &["meeting", "summary", "notes", "calendar", "agenda", "action", "follow-up"],
        min_score: 30,
    },
    AgentCatalogEntry {
        id: "document-qa",
        name: "Document Q&A Agent",
        domain: "operations",
        base_hours: "15-25",
        tier: "growth",
        keywords: &["document", "qa", "search", "find", "policy", "procedure", "handbook"],
        min_score: 45,
    },
    // Customer Service domain
    AgentCatalogEntry {
        id: "ticket-router",
        name: "Ticket Routing Agent",
        domain: "customer_service",
        base_hours: "10-20",
        tier: "starter",
        keywords: &["ticket", "routing", "support", "assign", "queue", "priority", "customer"],
        min_score: 35,
    },
    AgentCatalogEntry {
        id: "faq-responder",
        name: "FAQ Responder",
        domain: "customer_service",
        base_hours: "8-15",
        tier: "starter",
        keywords: &["faq", "answer", "automate", "common", "question", "response", "self-service"],
        min_score: 30,
    },
    // Finance domain
    AgentCatalogEntry {
        id: "invoice-processor",
        name: "Invoice Processing Agent",
        domain: "finance",
        base_hours: "12-22",
        tier: "growth",
        keywords: &["invoice", "billing", "process", "ap", "payment", "vendor", "expense"],
        min_score: 40,
    },
    AgentCatalogEntry {
        id: "report-generator",
        name: "Report Generator",
        domain: "finance",
        base_hours: "18-28",
        tier: "growth",
        keywords: &["report", "financial", "analytics", "dashboard", "kpi", "metrics", "data"],
        min_score: 50,
    },
];

/// Main recommendation function
pub fn recommend_agents(req: &RecommendationRequest) -> Vec<AgentRecommendation> {
    let mut recommendations = Vec::new();

    // Find domain score for the current request's domains
    for agent in AGENT_CATALOG {
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

        // Estimate savings
        let savings_hours = estimate_hours_saved(agent.base_hours, &req.team_size);
        let savings_inr = estimate_savings_inr(agent.base_hours, &req.team_size);

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
            savings_inr: savings_inr.clone(),
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
    match req.team_size.as_str() {
        "1-10" => reason.push_str("Suitable for small teams."),
        "11-50" => reason.push_str("Optimized for growing teams."),
        "51-200" => reason.push_str("Scalable for mid-size organizations."),
        "201+" => reason.push_str("Enterprise-grade solution."),
        _ => reason.push_str("Flexible for any team size."),
    }

    reason
}

/// Estimate hours saved per month based on base hours and team size
fn estimate_hours_saved(base_hours: &str, team_size: &str) -> String {
    let (min, max) = parse_hours_range(base_hours);
    let multiplier = team_size_multiplier(team_size);

    let adjusted_min = (min as f64 * multiplier) as u32;
    let adjusted_max = (max as f64 * multiplier) as u32;

    format!("{}-{}", adjusted_min, adjusted_max)
}
/// Estimate savings in INR (monthly)
fn estimate_savings_inr(base_hours: &str, team_size: &str) -> String {
    let (min, max) = parse_hours_range(base_hours);
    let multiplier = team_size_multiplier(team_size);

    // Assume average cost of ₹2,000 per hour for business operations
    let hourly_rate = 2000.0;

    let min_savings = (min as f64 * multiplier * hourly_rate) as u64;
    let max_savings = (max as f64 * multiplier * hourly_rate) as u64;

    format!("₹{}-₹{}", min_savings, max_savings)
}

/// Parse hours range string like "15-25" into (min, max)
fn parse_hours_range(hours_str: &str) -> (u32, u32) {
    let parts: Vec<&str> = hours_str.split('-').collect();
    if parts.len() == 2 {
        let min = parts[0].parse().unwrap_or(0);
        let max = parts[1].parse().unwrap_or(0);
        (min, max)
    } else {
        (0, 0)
    }
}

/// Get multiplier based on team size
fn team_size_multiplier(team_size: &str) -> f64 {
    match team_size {
        "1-10" => 1.0,
        "11-50" => 1.5,
        "51-200" => 2.0,
        "201+" => 2.5,
        _ => 1.0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_hours_range() {
        assert_eq!(parse_hours_range("15-25"), (15, 25));
        assert_eq!(parse_hours_range("8-15"), (8, 15));
        assert_eq!(parse_hours_range("invalid"), (0, 0));
    }

    #[test]
    fn test_team_size_multiplier() {
        assert_eq!(team_size_multiplier("1-10"), 1.0);
        assert_eq!(team_size_multiplier("11-50"), 1.5);
        assert_eq!(team_size_multiplier("51-200"), 2.0);
        assert_eq!(team_size_multiplier("201+"), 2.5);
        assert_eq!(team_size_multiplier("unknown"), 1.0);
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
        // deployment-monitor and meeting-summarizer have min_score 35 and 30 respectively, but no keyword match
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
    fn test_estimate_savings_inr() {
        let result = estimate_savings_inr("15-25", "1-10");
        assert!(result.starts_with("₹"));
        assert!(result.contains('-'));
    }

    #[test]
    fn test_estimate_hours_saved() {
        let result = estimate_hours_saved("15-25", "11-50");
        assert!(result.contains('-'));
        let parts: Vec<&str> = result.split('-').collect();
        assert_eq!(parts.len(), 2);
        let min: u32 = parts[0].parse().unwrap();
        let max: u32 = parts[1].parse().unwrap();
        // For 11-50 (multiplier 1.5), 15*1.5=22.5->22, 25*1.5=37.5->37
        assert_eq!(min, 22);
        assert_eq!(max, 37);
    }
}
