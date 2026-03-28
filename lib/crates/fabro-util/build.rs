use serde::Deserialize;
use std::env;
use std::fmt::Write;
use std::fs;
use std::path::Path;

#[derive(Deserialize)]
struct Config {
    allowlist: Option<GlobalAllowlist>,
    rules: Vec<Rule>,
}

#[derive(Deserialize)]
struct GlobalAllowlist {
    regexes: Option<Vec<String>>,
    stopwords: Option<Vec<String>>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Rule {
    id: String,
    regex: String,
    #[serde(default)]
    keywords: Vec<String>,
    entropy: Option<f64>,
    #[serde(default)]
    allowlist: Option<RuleAllowlist>,
    #[allow(dead_code)]
    description: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RuleAllowlist {
    regexes: Option<Vec<String>>,
    stopwords: Option<Vec<String>>,
    regex_target: Option<String>,
}

fn escape_rust_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 10);
    for ch in s.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c => out.push(c),
        }
    }
    out
}

fn main() {
    println!("cargo:rerun-if-changed=data/gitleaks.toml");

    let toml_path = Path::new("data/gitleaks.toml");
    let toml_content = fs::read_to_string(toml_path).expect("failed to read gitleaks.toml");
    let config: Config = toml::from_str(&toml_content).expect("failed to parse gitleaks.toml");

    let out_dir = env::var("OUT_DIR").unwrap();
    let out_path = Path::new(&out_dir).join("rules_generated.rs");

    let mut code = String::new();

    // Global allowlist regexes
    code.push_str("#[allow(unreachable_pub)]\npub const GLOBAL_ALLOWLIST_REGEXES: &[&str] = &[\n");
    if let Some(ref al) = config.allowlist {
        if let Some(ref regexes) = al.regexes {
            for r in regexes {
                let _ = writeln!(code, "    \"{}\",", escape_rust_string(r));
            }
        }
    }
    code.push_str("];\n\n");

    // Global allowlist stopwords
    code.push_str(
        "#[allow(unreachable_pub)]\npub const GLOBAL_ALLOWLIST_STOPWORDS: &[&str] = &[\n",
    );
    if let Some(ref al) = config.allowlist {
        if let Some(ref stopwords) = al.stopwords {
            for sw in stopwords {
                let _ = writeln!(code, "    \"{}\",", escape_rust_string(sw));
            }
        }
    }
    code.push_str("];\n\n");

    // Rule definitions
    code.push_str("#[allow(dead_code, unreachable_pub)]\n");
    code.push_str("pub struct RuleDef {\n");
    code.push_str("    pub id: &'static str,\n");
    code.push_str("    pub regex_pattern: &'static str,\n");
    code.push_str("    pub keywords: &'static [&'static str],\n");
    code.push_str("    pub entropy: Option<f64>,\n");
    code.push_str("    pub allowlist_regexes: &'static [&'static str],\n");
    code.push_str("    pub allowlist_stopwords: &'static [&'static str],\n");
    code.push_str("    pub allowlist_regex_target: Option<&'static str>,\n");
    code.push_str("}\n\n");

    code.push_str("#[allow(unreachable_pub)]\npub const RULES: &[RuleDef] = &[\n");

    for rule in &config.rules {
        code.push_str("    RuleDef {\n");
        let _ = writeln!(code, "        id: \"{}\",", escape_rust_string(&rule.id));
        let _ = writeln!(
            code,
            "        regex_pattern: \"{}\",",
            escape_rust_string(&rule.regex)
        );

        // Keywords
        code.push_str("        keywords: &[");
        for kw in &rule.keywords {
            let _ = write!(code, "\"{}\", ", escape_rust_string(kw));
        }
        code.push_str("],\n");

        // Entropy
        match rule.entropy {
            Some(e) => {
                let _ = writeln!(code, "        entropy: Some({:.1}),", e);
            }
            None => code.push_str("        entropy: None,\n"),
        }

        // Allowlist regexes
        code.push_str("        allowlist_regexes: &[");
        if let Some(ref al) = rule.allowlist {
            if let Some(ref regexes) = al.regexes {
                for r in regexes {
                    let _ = write!(code, "\"{}\", ", escape_rust_string(r));
                }
            }
        }
        code.push_str("],\n");

        // Allowlist stopwords
        code.push_str("        allowlist_stopwords: &[");
        if let Some(ref al) = rule.allowlist {
            if let Some(ref stopwords) = al.stopwords {
                for sw in stopwords {
                    let _ = write!(code, "\"{}\", ", escape_rust_string(sw));
                }
            }
        }
        code.push_str("],\n");

        // Allowlist regex target
        if let Some(ref al) = rule.allowlist {
            if let Some(ref target) = al.regex_target {
                let _ = writeln!(
                    code,
                    "        allowlist_regex_target: Some(\"{}\"),",
                    escape_rust_string(target)
                );
            } else {
                code.push_str("        allowlist_regex_target: None,\n");
            }
        } else {
            code.push_str("        allowlist_regex_target: None,\n");
        }

        code.push_str("    },\n");
    }

    code.push_str("];\n");

    fs::write(&out_path, code).expect("failed to write generated rules");
}
