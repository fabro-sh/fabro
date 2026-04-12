use std::sync::{LazyLock, OnceLock};

use aho_corasick::AhoCorasick;
use regex::Regex;

use super::Region;

mod generated {
    include!(concat!(env!("OUT_DIR"), "/rules_generated.rs"));
}

struct LazyRule {
    #[allow(dead_code)]
    id: &'static str,
    pattern: &'static str,
    regex: OnceLock<Option<Regex>>,
    #[allow(dead_code)]
    keywords: &'static [&'static str],
    allowlist_regex_patterns: &'static [&'static str],
    allowlist_regexes: Vec<OnceLock<Option<Regex>>>,
    allowlist_stopwords: &'static [&'static str],
    allowlist_regex_target: Option<&'static str>,
}

impl LazyRule {
    fn regex(&self) -> Option<&Regex> {
        self.regex
            .get_or_init(|| Regex::new(self.pattern).ok())
            .as_ref()
    }

    fn secret_group(&self) -> usize {
        match self.regex() {
            Some(r) if r.captures_len() > 1 => 1,
            _ => 0,
        }
    }

    fn allowlist_regex(&self, index: usize) -> Option<&Regex> {
        self.allowlist_regexes[index]
            .get_or_init(|| Regex::new(self.allowlist_regex_patterns[index]).ok())
            .as_ref()
    }

    fn is_allowlisted(&self, target: &str) -> bool {
        (0..self.allowlist_regex_patterns.len())
            .any(|i| self.allowlist_regex(i).is_some_and(|r| r.is_match(target)))
    }
}

struct GitleaksEngine {
    keyword_filter: AhoCorasick,
    /// For each keyword, which rule indices use it.
    keyword_to_rules: Vec<Vec<usize>>,
    /// Rules that have no keywords (must always be checked).
    no_keyword_rules: Vec<usize>,
    rules: Vec<LazyRule>,
    global_allowlist_regexes: Vec<OnceLock<Option<Regex>>>,
    global_allowlist_stopwords: &'static [&'static str],
}

impl GitleaksEngine {
    fn build() -> Option<Self> {
        let mut rules = Vec::new();
        let mut all_keywords: Vec<String> = Vec::new();
        let mut keyword_to_rules: Vec<Vec<usize>> = Vec::new();
        let mut no_keyword_rules = Vec::new();

        for def in generated::RULES {
            let rule_idx = rules.len();

            if def.keywords.is_empty() {
                no_keyword_rules.push(rule_idx);
            } else {
                for kw in def.keywords {
                    let kw_lower = kw.to_lowercase();
                    if let Some(pos) = all_keywords.iter().position(|k| k == &kw_lower) {
                        keyword_to_rules[pos].push(rule_idx);
                    } else {
                        all_keywords.push(kw_lower);
                        keyword_to_rules.push(vec![rule_idx]);
                    }
                }
            }

            let allowlist_regexes = def
                .allowlist_regexes
                .iter()
                .map(|_| OnceLock::new())
                .collect();

            rules.push(LazyRule {
                id: def.id,
                pattern: def.regex_pattern,
                regex: OnceLock::new(),
                keywords: def.keywords,
                allowlist_regex_patterns: def.allowlist_regexes,
                allowlist_regexes,
                allowlist_stopwords: def.allowlist_stopwords,
                allowlist_regex_target: def.allowlist_regex_target,
            });
        }

        let keyword_filter = AhoCorasick::builder()
            .ascii_case_insensitive(true)
            .build(&all_keywords)
            .ok()?;

        let global_allowlist_regexes = generated::GLOBAL_ALLOWLIST_REGEXES
            .iter()
            .map(|_| OnceLock::new())
            .collect();

        Some(Self {
            keyword_filter,
            keyword_to_rules,
            no_keyword_rules,
            rules,
            global_allowlist_regexes,
            global_allowlist_stopwords: generated::GLOBAL_ALLOWLIST_STOPWORDS,
        })
    }

    fn global_allowlist_regex(&self, index: usize) -> Option<&Regex> {
        self.global_allowlist_regexes[index]
            .get_or_init(|| Regex::new(generated::GLOBAL_ALLOWLIST_REGEXES[index]).ok())
            .as_ref()
    }

    fn find_regions(&self, s: &str) -> Vec<Region> {
        // Determine which rules to check based on keyword matches.
        let s_lower = s.to_lowercase();
        let mut rule_indices: Vec<bool> = vec![false; self.rules.len()];

        // Always include rules with no keywords.
        for &idx in &self.no_keyword_rules {
            rule_indices[idx] = true;
        }

        // Use overlapping search to ensure short keywords (e.g. "sk") don't
        // prevent longer overlapping keywords (e.g. "sk_test") from matching.
        for mat in self.keyword_filter.find_overlapping_iter(&s_lower) {
            for &rule_idx in &self.keyword_to_rules[mat.pattern().as_usize()] {
                rule_indices[rule_idx] = true;
            }
        }

        let mut regions = Vec::new();

        for (idx, should_check) in rule_indices.iter().enumerate() {
            if !should_check {
                continue;
            }
            let rule = &self.rules[idx];

            // Lazy-compile the regex on first use; skip if invalid.
            let Some(regex) = rule.regex() else {
                continue;
            };
            let secret_group = rule.secret_group();

            for caps in regex.captures_iter(s) {
                let full_match = caps.get(0).unwrap();

                // Get the secret: group 1 if it exists, otherwise full match
                let secret_match = if secret_group > 0 {
                    match caps.get(secret_group) {
                        Some(m) => m,
                        None => continue,
                    }
                } else {
                    full_match
                };

                let secret = secret_match.as_str();
                if secret.is_empty() {
                    continue;
                }

                // Check global allowlist regexes against the secret
                let globally_allowlisted =
                    (0..generated::GLOBAL_ALLOWLIST_REGEXES.len()).any(|i| {
                        self.global_allowlist_regex(i)
                            .is_some_and(|r| r.is_match(secret))
                    });
                if globally_allowlisted {
                    continue;
                }

                // Check global allowlist stopwords
                if self.global_allowlist_stopwords.contains(&secret) {
                    continue;
                }

                // Check rule-level allowlist
                let allowlist_target = if rule.allowlist_regex_target == Some("match") {
                    full_match.as_str()
                } else {
                    secret
                };

                if rule.is_allowlisted(allowlist_target) {
                    continue;
                }

                // Check rule-level stopwords
                let secret_lower = secret.to_lowercase();
                if rule
                    .allowlist_stopwords
                    .iter()
                    .any(|sw| secret_lower == *sw)
                {
                    continue;
                }

                regions.push(Region {
                    start: secret_match.start(),
                    end:   secret_match.end(),
                });
            }
        }

        regions
    }
}

static ENGINE: LazyLock<Option<GitleaksEngine>> = LazyLock::new(GitleaksEngine::build);

/// Find regions matching gitleaks rules.
pub(super) fn find_gitleaks_regions(s: &str) -> Vec<Region> {
    match ENGINE.as_ref() {
        Some(engine) => engine.find_regions(s),
        None => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_aws_access_key() {
        let regions = find_gitleaks_regions("key=AKIAYRWQG5EJLPZLBYNP");
        assert_eq!(regions.len(), 1, "expected 1 region, got {regions:?}");
        assert_eq!(
            &"key=AKIAYRWQG5EJLPZLBYNP"[regions[0].start..regions[0].end],
            "AKIAYRWQG5EJLPZLBYNP"
        );
    }

    #[test]
    fn detects_github_pat() {
        // ghp_ + exactly 36 alphanumeric chars
        let input = "ghp_ABCDEFGHIJKLMNOPQRSTUVWXYZabcdef0123";
        assert_eq!(input.len(), 40); // ghp_(4) + 36 = 40
        let regions = find_gitleaks_regions(input);
        assert_eq!(regions.len(), 1, "expected 1 region, got {regions:?}");
        assert_eq!(&input[regions[0].start..regions[0].end], input);
    }

    #[test]
    fn detects_stripe_key() {
        // Stripe regex requires trailing whitespace/punctuation or end of string
        let input = "sk_test_4eC39HqLyjWDarjtT1zdp7dc ";
        let regions = find_gitleaks_regions(input);
        assert_eq!(regions.len(), 1, "expected 1 region, got {regions:?}");
    }

    #[test]
    fn detects_private_key_block() {
        let input =
            "-----BEGIN RSA PRIVATE KEY-----\nMIIEpAIBAAKCAQEA\n-----END RSA PRIVATE KEY-----";
        let regions = find_gitleaks_regions(input);
        assert_eq!(regions.len(), 1, "expected 1 region, got {regions:?}");
    }

    #[test]
    fn normal_text_not_flagged() {
        let regions =
            find_gitleaks_regions("Hello, this is a normal English sentence with no secrets.");
        assert!(regions.is_empty(), "expected no regions, got {regions:?}");
    }

    #[test]
    fn global_allowlist_stopwords_respected() {
        // The global allowlist includes a UUID that should not be flagged
        let regions = find_gitleaks_regions("014df517-39d1-4453-b7b3-9930c563627c");
        assert!(
            regions.is_empty(),
            "expected no regions for allowlisted UUID, got {regions:?}"
        );
    }

    #[test]
    fn aws_example_key_not_flagged() {
        // AKIAIOSFODNN7EXAMPLE ends with EXAMPLE — per-rule allowlist
        let regions = find_gitleaks_regions("key=AKIAIOSFODNN7EXAMPLE");
        assert!(
            regions.is_empty(),
            "expected no regions for example key, got {regions:?}"
        );
    }
}
